//! The assembly function: the pure step that turns provider output into the
//! final, ordered, capped result list. This is where routing, aliases,
//! learning and ranking — each built in an earlier M1 slice — meet for the
//! first time.
//!
//! No disk reads, subprocess spawns, or network calls happen anywhere in
//! this module: [`Pipeline::assemble`] runs on every keystroke.
//!
//! It is also where an item's self-asserted `kind` and `provider` stop being
//! taken on trust. Items reach assembly as [`CheckedItems`] — built only by
//! [`CheckedItems::check`], from each producing provider's
//! [`ProviderOutput`] — so every item ranked here was vouched for by the
//! manifest of the provider that actually produced it, and the ones that
//! weren't come back as [`Rejection`]s.
//!
//! Not every rejection is such an item, though, and reading one as proof that
//! a provider lied would be wrong: assembly also refuses items it was
//! perfectly satisfied with, when a query asks for more pinned rows than the
//! **pin budget** honors — either half of it, so see both
//! [`MAX_PINNED_ITEMS_PER_PROVIDER`] and [`MAX_PINNED_ITEMS_PER_QUERY`]. A rejection
//! carries the [`FailedCheck`] that produced it precisely so the two are told
//! apart.

use hop_protocol::limits::{
    MAX_ACTION_LABEL, MAX_ACTIONS_PER_ITEM, MAX_COPY_TEXT, MAX_SUBTITLE, MAX_TITLE,
};
use hop_protocol::{Item, ItemId, Kind};

use crate::aliases::Aliases;
use crate::learning::Learning;
use crate::provider::{Provider, ProviderManifest};
use crate::rank::{Boosts, Ranker, Weights};
use crate::router::{Mode, RoutedQuery, route};

/// One provider's answer to one query, still attached to the manifest of the
/// provider that produced it.
///
/// An [`Item`] describes its own `kind` and its own `provider`, and nothing
/// downstream can tell a truthful self-description from a forged one on the
/// item alone. The association between a producer and what it produced is
/// known only at the moment a provider returns — a scheduler that flattens
/// every provider's items into one `Vec<Item>` destroys it, and no amount of
/// care further down can reconstruct it. So the association travels: this
/// type is what a scheduler hands to [`CheckedItems::check`], one value per
/// provider that answered.
///
/// ## Why the manifest cannot be supplied by the caller
///
/// Both fields are private and [`ProviderOutput::from_provider`] is the only
/// constructor, because a manifest a caller can name is a manifest a forged
/// item can select. The failure that shape invites is not hypothetical: a
/// scheduler holding a flat `Vec<Item>` would naturally group it for checking
/// by reading each item's own `provider` string and looking the matching
/// manifest up by that id — at which point both checks are tautologies. The
/// provenance check would compare a claimed id against a manifest chosen *by*
/// that claimed id, and the kind check would run against the impersonated
/// provider's declared kinds. Every abuse in issue #31 would be back, with
/// the checks still nominally in place.
///
/// Taking the dispatched [`Provider`] itself removes the string from the
/// path: the manifest comes from [`Provider::manifest`] on the object that
/// was asked, so nothing an item says about itself can influence which
/// manifest it is checked against. The one freedom left to a caller is which
/// provider object it hands over alongside which items, and that is a pairing
/// made where the provider is in hand — not something derivable from item
/// data, and not something `dyn Provider` can launder either, since
/// [`Provider`]'s RPITIT methods make it dyn-incompatible by construction.
///
/// The manifest is owned rather than borrowed because [`Provider::manifest`]
/// returns a fresh value. That is two small allocations per provider per
/// query — `ProviderManifest`'s clone copies both its `kinds` and `modes`
/// `Vec`s — on a path that then fuzzy-matches every item that provider
/// returned.
#[derive(Debug)]
pub struct ProviderOutput {
    manifest: ProviderManifest,
    items: Vec<Item>,
}

impl ProviderOutput {
    /// Pairs `items` with the manifest of the provider that produced them,
    /// asking `provider` for that manifest directly. See the type's docs for
    /// why this is the only way to build one.
    ///
    /// `items` is what this provider's own [`Provider::query`] returned;
    /// dispatching providers, honouring their budgets and collecting their
    /// answers is M2 daemon work that happens upstream of this crate.
    ///
    /// ## When the manifest is read, and what that costs
    ///
    /// Now — *after* `query` has already returned. This call is the only
    /// [`Provider::manifest`] call anywhere on this crate's path, so the
    /// manifest an item is checked against is whatever the provider chooses
    /// to answer with at check time, and nothing here can tell that apart
    /// from the manifest the same provider gave at registration. That the two
    /// agree is [`Provider::manifest`]'s documented stability requirement —
    /// a contract this constructor rests on and does not enforce. Read that
    /// method's docs for the abuse a provider that ignores it recovers.
    ///
    /// It cannot be enforced from here, by design: this constructor never
    /// sees a manifest captured any earlier than the call it makes, so it has
    /// nothing of its own to compare against. `hop-core` now has both a
    /// registry and a scheduler —
    /// [`ProviderHost`](crate::host::ProviderHost) — and its private
    /// `run_one` is in the strictly stronger position this paragraph used to
    /// ask a future host for: it keeps a manifest captured once at
    /// registration, which cannot be re-minted in response to what a provider
    /// decided to return, and it compares that captured manifest against the
    /// one this constructor reads back through `ProviderOutput::manifest`,
    /// refusing the provider's whole answer on any mismatch. What it does
    /// not do, and must not, is hand its captured manifest to *this*
    /// constructor to be checked against: a constructor taking a
    /// caller-supplied manifest is the hole the section above exists to keep
    /// closed, and it does not stop being that hole because this particular
    /// caller would have passed a trustworthy value. The host's comparison
    /// runs beside this constructor, on the value it returns — never inside
    /// it.
    pub fn from_provider<P: Provider>(provider: &P, items: Vec<Item>) -> Self {
        ProviderOutput {
            manifest: provider.manifest(),
            items,
        }
    }

    /// The manifest this value was actually built with — the one
    /// [`CheckedItems::check`] checks `items` against.
    ///
    /// This is not a second way to supply a manifest, and does not reopen the
    /// hole the type's docs describe: it reads back the value
    /// [`ProviderOutput::from_provider`] already minted from
    /// [`Provider::manifest`], rather than accepting one from a caller. A
    /// host that wants to catch a provider whose manifest shifted between its
    /// own captured copy and the call this constructor made needs to compare
    /// against *that* call specifically — not an earlier or later one — and
    /// this is the only way to read it back once the value has been built.
    /// `pub(crate)` because the need is `hop-core`-internal
    /// ([`crate::host::ProviderHost`]); nothing downstream of this crate has
    /// a captured manifest of its own to compare against.
    pub(crate) fn manifest(&self) -> &ProviderManifest {
        &self.manifest
    }
}

/// The **pin budget**'s per-provider half: the most `append_to_end` items one
/// producing provider is honored for on a single query. Together with
/// [`MAX_PINNED_ITEMS_PER_QUERY`] this is the whole budget, and a pinned item
/// over either half is refused as a [`FailedCheck::PinBudget`] rejection.
///
/// ## What the pinned tail bypasses, and why it needs a budget at all
///
/// The pinned tail is the one path into the result list that nothing filters.
/// Step 4 of [`Pipeline::assemble`] splits every `append_to_end` item off
/// before the exclusive-mode filter (step 5) and before [`Ranker::rank`] —
/// with its fuzzy match and its `min_score` floor — so neither ever sees one,
/// and an item the budget then honors arrives in the list having faced
/// nothing. That bypass is deliberate, and it is what makes the pinned
/// web-search row work: the row must show for `w firefox` even though nothing
/// about it matches "firefox", and it must show under an exclusive route whose
/// kinds it does not share.
///
/// `append_to_end` is a plain `bool` on [`Item`] though, and any provider can
/// set it on every item it returns. Setting it is therefore a *request* for
/// the tail, not entry to it. Unbudgeted, every request would be granted: that
/// is guaranteed placement on every query for as many items as a provider
/// cares to send, filling whatever the ranked body leaves under `max_results`.
/// The budget keeps the exception the size of its intended use rather than the
/// size of whatever a provider asks for.
///
/// ## Why one, and why this is the half that carries the weight
///
/// The argument is about what the path is *for*, not about how many providers
/// exist to use it — none ship in this repo yet, and a number chosen to fit
/// today's implementations would be a number chosen from nothing. What the
/// path is for is placing a row that ranking cannot reach: the web-search
/// action stands for "do this with what you typed", so it must appear on a
/// query it does not match. One row per source is exactly that need. A second
/// unmatched row from the same source is no longer a standing offer but a
/// list, and a list of things the user might want is what ranking is for — so
/// the second row belongs in the ranked body, as an *unflagged* item. That is
/// the provider's move to make and not something assembly does on its behalf:
/// step 4 refuses an over-share pin outright and never demotes one into the
/// body, and only an item that did not ask for the tail reaches the body at
/// all.
///
/// So one is the number at which no *single* source can assemble a collection
/// of unrankable rows. It is not what stops a collection arriving from several
/// sources at once: three sources inside this half still put three unfiltered
/// rows in the list, and holding that number down is
/// [`MAX_PINNED_ITEMS_PER_QUERY`]'s job rather than this one's. What this half
/// guarantees is narrower, and worth stating exactly: no source is shut out of
/// the pinned path *by this half*, however many rows it asks for. Being shut
/// out altogether is something only the per-query total does, to whichever
/// sources come after it is spent.
///
/// It is also the only value that needs no tie-break within a source. At two
/// or more, something must decide which of a source's own pins survive, and
/// the only order available is the one the source itself supplied — which
/// hands the choice back to the provider the share exists to limit.
///
/// And it is what keeps a flooding provider from taking the pinned path *for
/// itself*. Because the share is per producer, a provider that flags a hundred
/// items is refused its second pin for having had its first, not for being who
/// it is: the refusal is arithmetic over producer ids, needing no notion of
/// privilege. A genuine web-search row behind such a flooder therefore still
/// lands — provided the per-query total has a slot left for it, which is the
/// qualification this half cannot make on its own.
/// `tests::a_flooding_provider_that_answers_first_cannot_crowd_out_another_providers_pin`
/// is that case with one flooder ahead of the row;
/// `tests::a_fourth_provider_is_refused_once_the_query_total_is_spent` is the
/// case where the total has run out first. That is the guarantee worth having,
/// and it is why this half — not the per-query total — is the load-bearing one.
pub const MAX_PINNED_ITEMS_PER_PROVIDER: usize = 1;

/// The **pin budget**'s per-query half: the most `append_to_end` items
/// [`Pipeline::assemble`] honors in total for one query, across every provider
/// that answered. See [`MAX_PINNED_ITEMS_PER_PROVIDER`] for the other half and
/// for what the pinned tail bypasses.
///
/// ## Why three
///
/// This half is a judgement call, and worth marking as one: with
/// [`MAX_PINNED_ITEMS_PER_PROVIDER`] already holding every provider to a
/// single row, the total only decides how many *different* providers may pin
/// on one query before the rest are refused. Three is enough for the
/// first-party web-search row and two rows of the same shape from *other*
/// providers (a second search engine, a "search in files" action) without a
/// constant bump — other providers because
/// [`MAX_PINNED_ITEMS_PER_PROVIDER`] means a second row from the same one is
/// refused however much room this total has left — and small enough that the
/// count of unfiltered rows stays something a reader of this module can hold
/// in their head.
///
/// What it deliberately does *not* claim is a share of the visible list.
/// `max_results` is a caller's argument, not a constant — see
/// [`MAX_ITEMS_PER_RESULTS_FRAME`](hop_protocol::limits::MAX_ITEMS_PER_RESULTS_FRAME),
/// which says the same of the frame it bounds — so no ratio of pinned rows to
/// ranked ones can be promised from here. A caller passing `max_results: 3`
/// against an empty ranked body gets a list that is entirely pinned, and that
/// is the caller's cap doing it, not this constant failing. What this constant
/// promises is a count, and only a count.
///
/// ## Which pinned items win
///
/// The first in **provider-supplied order** that the budget can still afford:
/// the order [`CheckedItems::check`] preserved, which is the order the outputs
/// were given, each provider's items in the order that provider returned them.
/// The pinned tail is never scored, so there is no other order available to
/// choose by — scoring it to decide which pins survive would be ranking the
/// items whose whole point is not being ranked. A consequence worth stating
/// rather than leaving to be discovered: a provider that reorders its own
/// output chooses which of its own pins survives.
///
/// ## Where a capability check would go
///
/// Here — these two constants and step 4's use of them are the stated place.
/// The budget counts pins; it has no notion of *who* may pin, so it cannot
/// prefer a first-party provider over a hostile one, and three providers
/// asking honestly for one row each is indistinguishable from three hostile
/// ones. What it does is make the question a smaller one: an unentitled
/// provider that wins a slot wins one unfiltered row rather than the list, and
/// every refusal is visible in [`Assembly::rejections`] rather than silent.
/// What stays open is this half, not the other: three providers pinning
/// honestly leave a fourth refused, and which three they are is
/// provider-supplied order — so a hostile provider can still cost a fourth
/// provider its row, though never more than one row and never the whole tail.
/// Deciding who is *entitled* to a slot is a capability check, and designing
/// one is out of scope for a budget.
///
/// ## What the pin budget gives up
///
/// A legitimate fourth pinned provider, refused for being fourth, and an
/// honest provider's legitimate second row — the sharper of the two, since
/// [`MAX_PINNED_ITEMS_PER_PROVIDER`] is 1 and a web-search provider that
/// wanted to offer two engines as two rows must now offer one. The
/// alternative was to leave the pinned path unbudgeted and rely on providers
/// being first-party, which is the assumption the flag's shape already breaks:
/// it is a field on a wire type, settable by anything that can answer a query.
/// A budget that occasionally costs an honest provider a row is cheaper than
/// an exception whose size is chosen by whoever abuses it, and the cost is
/// paid in a [`Rejection`] a caller can read rather than in a row that
/// vanishes.
pub const MAX_PINNED_ITEMS_PER_QUERY: usize = 3;

/// Maximum items [`CheckedItems::check`] accepts from one provider's single
/// [`ProviderOutput`] — one producer's answer to one query. Enforced by
/// truncating `output.items` to this many *before* the per-item loop begins,
/// so the loop itself, and every allocation it might do, is bounded to at
/// most this many iterations regardless of what a provider claims to send.
///
/// # Why the same number as a wire-frame cap, at a different layer
///
/// Deliberately the same value as
/// [`MAX_ITEMS_PER_RESULTS_FRAME`](hop_protocol::limits::MAX_ITEMS_PER_RESULTS_FRAME) —
/// reused, not coincidental, and not a shared constant either, because the two
/// bound different things at different points in the pipeline. That module's
/// cap bounds one **wire frame**, applied at deserialization on the
/// client-facing edge: no *client* need ever be shown more than that many
/// items at once. This constant bounds one **provider's answer**, applied
/// where that answer enters assembly — well before boosting, ranking, or a
/// results frame is ever built. Reusing the number says the same thing at
/// this earlier layer that the wire already says at the outer one: no single
/// provider should be able to hand assembly more raw material than a client
/// could ever legitimately be shown in one frame. Every provider that exists
/// today (the skeleton, the apps provider) answers with a handful to a few
/// hundred items; 1 000 is generous headroom, revisitable if a future bulk
/// provider (files, M5) needs its own pagination story — that provider would
/// do its own pre-filtering rather than dumping an entire index into one
/// `query()` answer.
///
/// # Truncate, not reject
///
/// The tail is silently dropped — the same "truncate-and-terminate, nothing
/// on the wire naming it" precedent `hopd::source`'s own accumulator already
/// uses for its own count cap
/// ([`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY)).
/// Unlike a field-length violation (see [`FailedCheck::FieldTooLong`]), an
/// item past this cap was never inspected at all, so there is nothing to
/// reject it *for*: the cap is about how much of a provider's answer assembly
/// is willing to look at, not about anything a dropped item did wrong.
///
/// This bounds what *assembly* does with a provider's answer. It does
/// nothing about the cost a hostile provider's own `query()` paid to build a
/// larger `Vec<Item>` before returning it — that cost is bounded elsewhere
/// ([`ProviderHost::run_one`](crate::host::ProviderHost)'s existing
/// budget/timeout enforcement) and is out of this constant's scope.
pub const MAX_ITEMS_PER_PROVIDER_ANSWER: usize = 1_000;

/// Which check an item failed, and so why assembly declined it. See
/// [`Rejection`].
///
/// Three of the four are checks [`CheckedItems::check`] runs against the
/// item itself, and all three are about a claim the item made — its `kind`,
/// its `provider`, or the size of one of its fields. The fourth is not a
/// claim at all: [`FailedCheck::PinBudget`] records an item assembly had no
/// room to honor. Read the variant before treating a rejection as evidence
/// that a provider lied — only [`FailedCheck::Kind`], [`FailedCheck::Provenance`]
/// and [`FailedCheck::FieldTooLong`] are that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedCheck {
    /// The item's `kind` is not among the producing provider's declared
    /// [`ProviderManifest::kinds`]. A provider declaring `kinds:
    /// [Calculator]` returning a `Kind::Window` item is the motivating abuse:
    /// the forged kind would have survived a `w `-exclusive filter and
    /// inherited Window's ranking weight.
    Kind,
    /// The item's `provider` string is not equal to the producing provider's
    /// [`ProviderManifest::id`]. The item claims to have come from somewhere
    /// it did not.
    Provenance,
    /// One of the item's variable-length fields is over the bound
    /// `hop_protocol::limits` already applies to that same field when it
    /// arrives by socket — `title` ([`MAX_TITLE`]), `subtitle`
    /// ([`MAX_SUBTITLE`]), `copy_text` ([`MAX_COPY_TEXT`]), an action's
    /// `label` ([`MAX_ACTION_LABEL`]), or the number of `actions`
    /// ([`MAX_ACTIONS_PER_ITEM`]). `field` names which one, as the same
    /// `Type.field` spelling `hop_protocol::limits`'s own deserializers use
    /// (e.g. `"Item.title"`, `"Action.label"`) — not a new naming scheme,
    /// so grepping a field name finds both layers that bound it.
    ///
    /// An item built in-process and never parsed off the wire had passed no
    /// length check at all until this variant existed — the gap
    /// [`hop_protocol::limits::MAX_ITEMS_PER_QUERY`]'s own docs used to call
    /// "documented, not enforced... wherever an item is built in-process".
    /// This is where it now is enforced, for the one seam every provider's
    /// answer must cross: [`CheckedItems::check`].
    FieldTooLong {
        /// Which field broke its bound, as `Type.field` — see this variant's
        /// own docs for the exact spelling used.
        field: &'static str,
    },
    /// The item is flagged `append_to_end` and the **pin budget** had nothing
    /// left to spend on it: either its producer already had its
    /// [`MAX_PINNED_ITEMS_PER_PROVIDER`] pins, or the query had already
    /// honored [`MAX_PINNED_ITEMS_PER_QUERY`] of them across every provider.
    /// Assembly refused it the pinned path rather than granting it placement
    /// no later step could take back.
    ///
    /// Unlike the three above, this says nothing about the item: it passed
    /// every check above it — an item any of those rejected never reaches
    /// the pinned path at all — and it is here only because its producer's
    /// share, or the query's total, was already spent. Which items spend the
    /// budget is provider-supplied order, so the same item can be honored on
    /// one query and refused on the next as other providers' answers change
    /// around it.
    PinBudget,
}

/// One item assembly refused, and why.
///
/// The four descriptive fields mean the same thing under every
/// [`FailedCheck`], but they read differently under
/// [`FailedCheck::PinBudget`]: that item passed every check
/// [`CheckedItems::check`] runs against an item itself (kind, provenance,
/// field length), so `claimed_provider` and `producer_id` are filled from
/// the same string and
/// are equal by construction. Their equality is not the interesting part and
/// proves nothing on its own. What the checks bought is that the string is the
/// producer's *real* manifest id rather than a claim the item made — the same
/// fact `producer_id` asserts everywhere, arrived at earlier.
///
/// Rejections are *returned as data* rather than logged from here, because
/// [`Pipeline::assemble`] is pure — it runs on every keystroke and may not
/// perform side effects. Everything here is owned, so a rejection outlives
/// both the item it describes and the borrow of the manifest that refused it:
/// a logging seam can move a `Vec<Rejection>` off the query path and format
/// it whenever it likes, without this type having to change shape. That seam
/// now exists — [`ProviderLog`](crate::host::ProviderLog) — and
/// [`ProviderHost::run_one`](crate::host::ProviderHost) is exactly that
/// caller: it reads the rejections [`CheckedItems::check`] produced for one
/// provider and records them as
/// [`ProviderEvent::Rejected`](crate::host::ProviderEvent::Rejected) before
/// this value's owned shape ever needs to matter to a query path with side
/// effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The rejected item's id.
    pub item_id: ItemId,
    /// The kind the rejected item claimed for itself.
    pub claimed_kind: Kind,
    /// The provider the rejected item claimed to come from — the forged
    /// value under [`FailedCheck::Provenance`].
    pub claimed_provider: String,
    /// The [`ProviderManifest::id`] of the provider that actually produced
    /// the item, which is what the claims above were checked against.
    pub producer_id: String,
    /// Which check failed. An item that fails more than one of
    /// [`CheckedItems::check`]'s three per-item checks (kind, provenance,
    /// field length) is reported once, against whichever runs first — see
    /// that function's `DECISION` comment.
    pub check: FailedCheck,
}

/// Items that have been checked against the manifest of the provider that
/// produced them, and the [`Rejection`]s from doing so.
///
/// ## Why this type exists at all
///
/// This is the only item collection [`Pipeline::assemble`] accepts, its
/// fields are private, and [`CheckedItems::check`] is its only constructor.
/// That shape is the enforcement: unchecked items cannot travel the assembly
/// path, because there is no way to build the value `assemble` demands except
/// by running the checks. A free function that returns `Vec<Item>` — or
/// public fields here — would leave the checks advisory, and a caller could
/// skip them by simply not calling, which is exactly the failure mode this
/// seam exists to remove. The compiler enforces it instead of a reviewer
/// noticing.
///
/// The guarantee is scoped to that seam, and deliberately not claimed for
/// scoring in general: [`Ranker::rank`] is public, takes a bare `Vec<Item>`,
/// and [`Pipeline::ranker`] is a public field, so `pipeline.ranker.rank(…)`
/// still reaches the fuzzy matcher and the title-dedupe with items no
/// manifest vouched for. What this type guarantees is that *assembly* — the
/// nine-step contract the daemon calls per query, where boosts, the exclusive
/// filter and the pinned tail all live — has no unchecked entrance.
///
/// The rejections ride along inside the value, and come back out in
/// [`Assembly`], rather than being handed back from `check` separately: what
/// assembly refused belongs to the query it refused them for, so one call
/// yields one outcome. It is worth being precise about what that does *not*
/// buy, since it would be easy to read as more: nothing obliges *this* caller
/// to look at them. [`Assembly`]'s fields are public and `.items` discards
/// the rejections in one character, which is exactly what the tests below do.
///
/// A logging seam that makes ignoring a rejection a real mistake now exists —
/// [`ProviderLog`](crate::host::ProviderLog), issue #34 — but it is reached
/// through [`ProviderHost::run_one`](crate::host::ProviderHost), which calls
/// [`CheckedItems::check`] directly and records what it returns; nothing
/// forces that same discipline on a caller of *this* type that isn't the
/// host. So the shape here keeps rejections available and attached to their
/// query, and the host is the caller that has made ignoring them a mistake —
/// it does not make them unignorable for every caller this type has.
#[derive(Debug, Clone)]
pub struct CheckedItems {
    items: Vec<Item>,
    rejections: Vec<Rejection>,
}

impl CheckedItems {
    /// Runs every per-item check over each provider's output, in the order
    /// the outputs were given, keeping each provider's items in the order
    /// that provider returned them — after first truncating each output's
    /// own item count to [`MAX_ITEMS_PER_PROVIDER_ANSWER`].
    ///
    /// An item is kept only if its `kind` is one its producer declared, its
    /// `provider` string equals its producer's manifest `id`, and none of its
    /// variable-length fields (`title`, `subtitle`, `copy_text`, an action's
    /// `label`, or the number of `actions`) is over the bound
    /// `hop_protocol::limits` already applies to that same field on the wire
    /// — see [`FailedCheck::FieldTooLong`]. Anything else becomes a
    /// [`Rejection`] and never reaches boosts, dedupe, filtering or ranking.
    ///
    /// The truncation runs *before* this loop even starts, not as one more
    /// condition inside it: a provider answering with far more than
    /// [`MAX_ITEMS_PER_PROVIDER_ANSWER`] items has the tail of `output.items`
    /// dropped first, so the loop below — and every allocation, comparison
    /// and possible [`Rejection`] it might produce — never runs more than
    /// that many times per output, regardless of what the provider claims to
    /// send. See [`MAX_ITEMS_PER_PROVIDER_ANSWER`] for why the cap is a
    /// truncation and not a rejection of the whole output.
    ///
    /// DECISION: an item that fails more than one of the three checks above
    /// is reported once, against the earliest of them to run — [`FailedCheck::Kind`],
    /// then [`FailedCheck::Provenance`], then [`FailedCheck::FieldTooLong`].
    /// A rejection identifies an item that is already gone; enumerating every
    /// way in which it lied would make the rejection list a variable-length
    /// report of a single event, for no gain to the only consumer it has (a
    /// future logging seam that wants to say what was dropped and why). The
    /// same one-report-per-item rule this comment already stated for the two
    /// original checks extends unchanged to the field-length check added
    /// alongside them: it is simply one more condition in the same chain,
    /// checked after the two that were already there.
    ///
    /// Note what this does *not* check: that the producing manifest itself is
    /// truthful. A provider that honestly declares `id: "evil"` and `kinds:
    /// [App]` can still return an item whose id collides with another
    /// provider's namespace. *Alias* boosts got that provider dimension in
    /// this branch (`Boosts::by_provider_item`, tagged via
    /// `AliasEffect::boosts`); learning boosts deliberately did not — see the
    /// DECISION at the learning-boost call site in `Pipeline::assemble`, and
    /// issue #72.
    pub fn check(outputs: Vec<ProviderOutput>) -> Self {
        let mut items = Vec::new();
        let mut rejections = Vec::new();

        for mut output in outputs {
            // Bounds the per-item loop below to at most this many iterations
            // for this output, before a single item is inspected — see
            // MAX_ITEMS_PER_PROVIDER_ANSWER for why this is a truncation of
            // the excess rather than a rejection of the whole answer.
            output.items.truncate(MAX_ITEMS_PER_PROVIDER_ANSWER);

            // Each item is checked against `output.manifest` and nothing
            // else. Hoisting the declared kinds or the ids out of this loop —
            // into one set spanning every provider that answered — would look
            // like a harmless optimisation and would silently restore both
            // abuses: any answering provider's kind would vouch for any item,
            // and any answering provider's id would satisfy provenance. See
            // `tests::an_item_is_checked_against_its_own_producer_not_the_union_of_every_manifest`.
            for item in output.items {
                let failed = if !output.manifest.kinds.contains(&item.kind) {
                    Some(FailedCheck::Kind)
                } else if item.provider != output.manifest.id {
                    Some(FailedCheck::Provenance)
                } else if item.title.len() > MAX_TITLE {
                    Some(FailedCheck::FieldTooLong {
                        field: "Item.title",
                    })
                } else if item
                    .subtitle
                    .as_ref()
                    .is_some_and(|subtitle| subtitle.len() > MAX_SUBTITLE)
                {
                    Some(FailedCheck::FieldTooLong {
                        field: "Item.subtitle",
                    })
                } else if item
                    .copy_text
                    .as_ref()
                    .is_some_and(|copy_text| copy_text.len() > MAX_COPY_TEXT)
                {
                    Some(FailedCheck::FieldTooLong {
                        field: "Item.copy_text",
                    })
                } else if item
                    .actions
                    .iter()
                    .any(|action| action.label.len() > MAX_ACTION_LABEL)
                {
                    Some(FailedCheck::FieldTooLong {
                        field: "Action.label",
                    })
                } else if item.actions.len() > MAX_ACTIONS_PER_ITEM {
                    Some(FailedCheck::FieldTooLong {
                        field: "Item.actions",
                    })
                } else {
                    None
                };

                match failed {
                    Some(check) => rejections.push(Rejection {
                        item_id: item.id,
                        claimed_kind: item.kind,
                        claimed_provider: item.provider,
                        producer_id: output.manifest.id.to_string(),
                        check,
                    }),
                    None => items.push(item),
                }
            }
        }

        CheckedItems { items, rejections }
    }

    /// The items that passed every check, in the order [`CheckedItems::check`]
    /// received them.
    ///
    /// A borrow, not a second route around the check: it lends what already
    /// exists rather than building anything, so it needs no justification
    /// against this type's "only [`check`](CheckedItems::check) may mint
    /// one" rule beyond that — nothing reachable through `&self` can ever
    /// have skipped it. What this exists *for* is an accumulator's per-
    /// arrival cap arithmetic: how many more items still fit before
    /// [`CheckedItems::truncate_items`] has anything to do, which is a
    /// question a length alone answers and owning the items would not
    /// answer any better.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The items that failed a check, in the order they were rejected.
    pub fn rejections(&self) -> &[Rejection] {
        &self.rejections
    }

    /// Appends `other`'s items and rejections onto the end of this value's
    /// own, both in order — the merge an accumulator needs to build the
    /// whole-query value [`Pipeline::assemble`] takes out of every
    /// provider's separately-checked answer, and the only way this crate
    /// offers to combine two [`CheckedItems`] into one.
    ///
    /// Safe against the "only `check` may mint one" rule because it never
    /// manufactures anything: both `self` and `other` already went through
    /// [`CheckedItems::check`], independently, each against its own
    /// producer's manifest, before either could exist to be passed here at
    /// all. Concatenating two already-checked lists cannot make an item
    /// checked that wasn't, and cannot un-check one that was — there is no
    /// third state for a merge to land in. What `absorb` does not do is
    /// re-verify anything: an accumulator that wants a newly arrived batch
    /// checked against *its* producer's manifest still has to call
    /// [`CheckedItems::check`] on that batch itself before absorbing the
    /// result; this method only ever receives values that already made it
    /// through that gate.
    pub fn absorb(&mut self, other: CheckedItems) {
        self.items.extend(other.items);
        self.rejections.extend(other.rejections);
    }

    /// Keeps at most `max` of `self`'s items, dropping the rest from the
    /// end, and leaves `self`'s rejections untouched.
    ///
    /// Safe for the same reason [`CheckedItems::absorb`] is: shortening an
    /// already-checked list adds nothing, so there is nothing here that
    /// could be unchecked. An item this drops was checked and simply not
    /// kept — never turned into a claim nothing verified, which is the only
    /// way this type's guarantee could actually break. Rejections are left
    /// alone on purpose: a rejection was never a candidate for this cap in
    /// the first place, it already recorded *why* a manifest check declined
    /// its item, and truncating it away here would misattribute that
    /// decision to this cap instead.
    pub fn truncate_items(&mut self, max: usize) {
        self.items.truncate(max);
    }
}

/// What [`Pipeline::assemble`] returns: the ordered, capped item list, and
/// every [`Rejection`] the same query produced.
#[derive(Debug)]
pub struct Assembly {
    /// The final result list: the ranked body followed by the pinned tail,
    /// truncated to the `max_results` the call asked for.
    pub items: Vec<Item>,
    /// Every item refused for this query: first the ones the manifest checks
    /// rejected, in the order [`CheckedItems::check`] rejected them, then the
    /// pinned items the **pin budget** could not afford, in provider-supplied
    /// order. Empty when every provider was honest about its own output and
    /// the query's pinned requests fit inside the budget — which is a claim
    /// about the query rather than about any one provider, since a fourth
    /// provider asking for a single row is over the budget just as surely as
    /// one provider asking for four. Nothing obliges a caller to read this —
    /// see [`CheckedItems`] on what that does
    /// and does not buy.
    pub rejections: Vec<Rejection>,
}

/// Wires together a [`Ranker`], [`Aliases`] table and [`Learning`] store —
/// each an M1 slice in its own right — into the one pure step the daemon
/// (and every test here) calls per query: [`Pipeline::assemble`].
///
/// `Default` builds all four fields from their own defaults, so a `Pipeline`
/// can be constructed without touching the filesystem — useful for tests and
/// for a future daemon that loads a persisted `Learning` separately and
/// swaps it in.
#[derive(Default)]
pub struct Pipeline {
    pub ranker: Ranker,
    pub aliases: Aliases,
    pub learning: Learning,
    pub weights: Weights,
}

/// The [`Kind`]s a given [`Mode`] serves — used by both the exclusive-mode
/// filter (step 5) and the inferred-mode promotion (step 7), so it's written
/// once. `Mode::All` deliberately returns `None`: it neither filters nor
/// promotes anything.
fn kinds_for_mode(mode: Mode) -> Option<&'static [Kind]> {
    match mode {
        Mode::Windows => Some(&[Kind::Window]),
        Mode::Apps => Some(&[Kind::App]),
        Mode::Files => Some(&[Kind::File]),
        Mode::Emoji => Some(&[Kind::Emoji]),
        Mode::Timezone => Some(&[Kind::Timezone]),
        Mode::Currency => Some(&[Kind::Currency]),
        Mode::Calculator => Some(&[Kind::Calculator]),
        Mode::Weather => Some(&[Kind::Weather]),
        Mode::Actions => Some(&[Kind::Action]),
        Mode::WebSearch => Some(&[Kind::WebSearch]),
        Mode::All => None,
    }
}

/// Stably moves every item whose kind is in `kinds` to the front of `items`,
/// preserving the relative order within each of the two groups and dropping
/// nothing. This is the augment-not-hijack rule from step 7: an inferred
/// utility result (e.g. a calculator hit for `2+2`) leads, but the rest of
/// the ranked body — including an app literally named `2048` — stays.
fn promote_kinds(items: &mut Vec<Item>, kinds: &[Kind]) {
    let (promoted, rest): (Vec<Item>, Vec<Item>) =
        items.drain(..).partition(|item| kinds.contains(&item.kind));
    items.extend(promoted);
    items.extend(rest);
}

impl Pipeline {
    /// Runs the pipeline's nine-step contract over one query's raw text and
    /// the items providers already returned for it. Provider *scheduling*
    /// (parallel dispatch, budgets, partial-result streaming) happens
    /// upstream of this call and is out of scope here — `assemble` is pure:
    /// same inputs, same output, no I/O.
    ///
    /// The items arrive as [`CheckedItems`], not as a `Vec<Item>`, and that
    /// is deliberate: an item's `kind` and `provider` are self-asserted, so
    /// every one of them has been checked against the manifest of the
    /// provider that actually produced it before this function can be called
    /// at all. See [`CheckedItems`] for why the constraint lives in the type
    /// rather than in a helper a caller could forget. The [`Rejection`]s that
    /// check produced come back out in [`Assembly::rejections`], including
    /// for `append_to_end` items — the pinned tail bypasses the exclusive
    /// filter (step 5), so an unchecked pinned item would be a hole straight
    /// through this. Step 4 adds rejections of its own to the same list, for
    /// the pinned items the **pin budget** could not afford.
    ///
    /// 1. Route `raw_query`.
    /// 2. Apply aliases to the routed term, producing `effective_term`
    ///    (what ranking uses) and any alias boosts.
    /// 3. Collect boosts: the alias boosts plus a learning boost for every
    ///    candidate item, summed into one [`Boosts`] map.
    /// 4. Split off the `append_to_end` items — the ones *requesting* the
    ///    pinned tail, never ranked (`Ranker::rank` drops them itself, so
    ///    this split is what keeps them alive at all) — and spend the **pin
    ///    budget** over the requests in provider-supplied order, the ones it
    ///    affords becoming the pinned tail: at most
    ///    [`MAX_PINNED_ITEMS_PER_PROVIDER`] from any one producer and at most
    ///    [`MAX_PINNED_ITEMS_PER_QUERY`] in all, with every request it cannot
    ///    afford rejected as [`FailedCheck::PinBudget`].
    /// 5. If the route is exclusive, filter the remaining items to that
    ///    mode's kinds.
    /// 6. Rank what remains, using `effective_term`.
    /// 7. If the mode was *inferred* (`!exclusive && mode != Mode::All`),
    ///    stably promote that mode's kinds to the front without removing
    ///    anything else.
    /// 8. Concatenate the pinned tail after the ranked body.
    /// 9. Truncate to `max_results` — see the comment above the truncate
    ///    call for why the cap counts the pinned tail too.
    pub fn assemble(
        &mut self,
        raw_query: &str,
        checked: CheckedItems,
        max_results: usize,
    ) -> Assembly {
        let CheckedItems {
            items: provider_items,
            mut rejections,
        } = checked;

        // Step 1: route.
        let routed = route(raw_query);

        // Step 2: apply aliases to the routed term.
        let alias_effect = self.aliases.apply(&routed.term);

        // Step 3: collect boosts — alias boosts plus a learning boost per
        // candidate item. Where both apply to the same item, they add.
        //
        // DECISION: the learning boost is keyed on `routed.term` — the
        // query after any prefix was stripped, but *before* the alias
        // rewrite above. An alias rewrite is a ranking substitution the user
        // never typed, so crediting it to learning would be recording a fact
        // that didn't happen. That distinction is the point here — not that
        // the term is the typed spelling, which it is not in every case:
        // routing canonicalizes an alias-matched timezone query, so
        // `sao paulo` and `SAO PAULO` share one learning key. See CONTEXT.md
        // on **Term**. This is a judgement call M2 may revisit once the daemon
        // records real launches and can observe how users actually expect
        // aliased queries to be learned from.
        let mut boosts = Boosts::default();
        for ((provider, id), boost) in &alias_effect.boosts {
            *boosts
                .by_provider_item
                .entry((provider.clone(), id.clone()))
                .or_insert(0.0) += *boost;
        }
        // DECISION: the learning boost stays keyed on the bare item id, with
        // no provider dimension, unlike the alias boost above. Issue #31's
        // boost-theft criterion is only *partially* met here on purpose —
        // `Learning::boost_for` sums `frequency_boost` (from the persisted
        // `global_frequency` map) and `query_boost` (from the per-query
        // `selections` map, kept in memory only, never written to disk), and
        // both are keyed on the bare id string. Giving `global_frequency` a
        // provider dimension is a persisted-format change, not an in-memory
        // rekey like `Boosts::by_provider_item` above: it means bumping
        // `hop-core`'s `learning::STORE_VERSION`, which that module answers
        // by refusing the older store rather than migrating it (see the
        // constant for why), so the cost is every user's learning, not a
        // migration to write. `selections` is deferred alongside it rather
        // than resolved on its own. Filed as issue #72.
        for item in &provider_items {
            let learned = self.learning.boost_for(&routed.term, &item.id);
            if learned != 0.0 {
                *boosts.by_item_id.entry(item.id.clone()).or_insert(0.0) += learned;
            }
        }

        // Step 4: split off the items requesting the pinned tail before
        // anything else touches the list — both the exclusive-mode filter
        // (step 5) and the ranker itself must never see these items — then
        // spend the pin budget over what asked for it.
        //
        // The pin budget is what stops an exception meant for one first-party
        // row from being a quantity any provider chooses, and its two halves
        // stop different things: MAX_PINNED_ITEMS_PER_PROVIDER stops one
        // producer taking the pinned path for itself, MAX_PINNED_ITEMS_PER_QUERY
        // stops enough providers doing it one pin at a time. See both
        // constants for what each half is worth, and MAX_PINNED_ITEMS_PER_QUERY
        // for where a capability check deciding *who* may pin belongs.
        //
        // A pin refused here is refused outright rather than left to step 9's
        // cap, because the two refuse different things: the cap drops what a
        // full list has no room for, while this refuses an item the pinned
        // path will not carry at all — so a pin over budget is rejected
        // whether or not the cap would have squeezed it out anyway.
        //
        // Asking `tail` itself which producers already have a pin is what
        // keeps the per-producer share from costing anything: `tail` holds at
        // most MAX_PINNED_ITEMS_PER_QUERY items, so the scan costs a constant
        // and the share needs no auxiliary map, no sort, and no second pass
        // over the requests. Spending the budget in one forward pass is also
        // what puts the rejections in provider-supplied order, which is the
        // order `Assembly::rejections` documents.
        //
        // `producer_id` is read off `item.provider` because these items came
        // out of CheckedItems: the provenance check established that an item's
        // `provider` string is its producer's manifest id, so what is read
        // here is the producer's real id rather than a claim. An item that
        // failed that check never reached this tail.
        let (requested, mut body): (Vec<Item>, Vec<Item>) = provider_items
            .into_iter()
            .partition(|item| item.append_to_end);
        let mut tail: Vec<Item> = Vec::new();
        for item in requested {
            let from_this_producer = tail.iter().filter(|p| p.provider == item.provider).count();
            if tail.len() >= MAX_PINNED_ITEMS_PER_QUERY
                || from_this_producer >= MAX_PINNED_ITEMS_PER_PROVIDER
            {
                rejections.push(Rejection {
                    item_id: item.id,
                    claimed_kind: item.kind,
                    claimed_provider: item.provider.clone(),
                    producer_id: item.provider,
                    check: FailedCheck::PinBudget,
                });
            } else {
                tail.push(item);
            }
        }

        // Step 5: an exclusive route filters the ranked body to its mode's
        // kinds. The pinned tail was already split off above, so a pinned
        // item this query honors survives an exclusive filter regardless of
        // its kind — see
        // `tests::pinned_item_survives_exclusive_filter_regardless_of_kind`.
        // The pin budget in step 4 is the only thing that can refuse it, and
        // it refuses on count alone, never on kind or score.
        if routed.exclusive
            && let Some(kinds) = kinds_for_mode(routed.mode)
        {
            body.retain(|item| kinds.contains(&item.kind));
        }

        // Step 6: rank the (possibly filtered) body against the effective
        // term. The routed query is otherwise unchanged — only `term`
        // differs from `routed`.
        let effective_query = RoutedQuery {
            term: alias_effect.effective_term,
            ..routed.clone()
        };
        let ranked = self
            .ranker
            .rank(body, &effective_query, &self.weights, &boosts);
        let mut ranked_items: Vec<Item> = ranked.into_iter().map(|r| r.item).collect();

        // Step 7: promote an *inferred* mode's kinds to the front, without
        // removing anything else. An explicit (exclusive) route was already
        // filtered down to exactly this mode's kinds in step 5 — promoting
        // again there would be a no-op at best and a bug-hiding no-op at
        // worst, which is why this is conditioned on `!exclusive`.
        if !routed.exclusive
            && routed.mode != Mode::All
            && let Some(kinds) = kinds_for_mode(routed.mode)
        {
            promote_kinds(&mut ranked_items, kinds);
        }

        // Step 8: concatenate the pinned tail after the ranked body.
        ranked_items.extend(tail);

        // Step 9: truncate to max_results.
        //
        // DECISION: truncation is plain — "concatenate, then truncate" with
        // nothing smarter. If the ranked body alone already reaches or
        // exceeds `max_results`, the pinned tail is squeezed out entirely
        // rather than the cap making room for it. No acceptance criterion
        // asks for reserved tail space, and this keeps the rule simple and
        // predictable: the cap is a hard ceiling on the whole list, not a
        // negotiation between its two halves. See
        // `tests::max_results_cap_squeezes_out_the_pinned_tail_when_the_ranked_body_alone_fills_it`.
        //
        // DIVERGENCE: the old extension's `combineRankedWithTail`
        // (`lib/searchResultsLayout.js`) does the opposite — it *reserves*
        // room for the tail by truncating the ranked body first, then
        // appending the tail (see `tests/search-results-layout.test.mjs`'s
        // "reserves space for tail rows within max results": 3 ranked + 2
        // tail capped at 3 yields 1 ranked + both tail rows). This slice
        // deliberately does not port that behavior — the issue specifies
        // "concatenate, then truncate" — so a cap that the ranked body fills
        // squeezes the tail out here, where the JS would have squeezed the
        // ranked body instead.
        ranked_items.truncate(max_results);
        Assembly {
            items: ranked_items,
            rejections,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::provider::{APPS_PROVIDER_ID, ProviderError, QueryCtx};
    use hop_protocol::{Action, ActionId, ActionKind, ExecOutcome, ItemId};
    use std::sync::Arc;
    use std::time::Duration;

    /// Every [`Kind`] there is. The `test` provider below declares all of
    /// them, so the ordering, filtering, promotion and truncation tests can
    /// keep using items of whatever kind the behaviour under test needs
    /// without each one having to stand up a provider of its own.
    const ALL_KINDS: [Kind; 10] = [
        Kind::App,
        Kind::Window,
        Kind::File,
        Kind::Calculator,
        Kind::Currency,
        Kind::Timezone,
        Kind::Weather,
        Kind::Emoji,
        Kind::WebSearch,
        Kind::Action,
    ];

    /// A provider that exists only to be a provider: [`ProviderOutput`] can
    /// be built no other way, so a test that wants to pair items with a
    /// manifest has to have something implementing [`Provider`] to ask. Its
    /// `query` is never called — assembly's input is items a provider has
    /// *already* returned, and these tests hand-write those items so they can
    /// forge the claims the checks are about.
    struct FakeProvider {
        manifest: ProviderManifest,
    }

    impl Provider for FakeProvider {
        fn manifest(&self) -> ProviderManifest {
            self.manifest.clone()
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Ok(Vec::new())
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    fn provider(id: &'static str, kinds: Vec<Kind>) -> FakeProvider {
        FakeProvider {
            manifest: ProviderManifest {
                id,
                kinds,
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(50),
            },
        }
    }

    /// One provider's answer: the items `id` claims to have produced, paired
    /// with `id`'s own manifest the only way [`ProviderOutput`] allows.
    fn output(id: &'static str, kinds: Vec<Kind>, items: Vec<Item>) -> ProviderOutput {
        ProviderOutput::from_provider(&provider(id, kinds), items)
    }

    /// Checks well-behaved output from the single fake provider most tests
    /// here share, and asserts nothing was rejected — so a test written about
    /// ordering can never quietly turn into a test about rejection.
    fn checked(items: Vec<Item>) -> CheckedItems {
        let checked = CheckedItems::check(vec![output("test", ALL_KINDS.to_vec(), items)]);
        assert!(
            checked.rejections().is_empty(),
            "this helper is for well-behaved provider output only"
        );
        checked
    }

    fn item(kind: Kind, id: &str, title: &str) -> Item {
        Item {
            id: ItemId::new(id).unwrap(),
            kind,
            title: title.to_string(),
            subtitle: None,
            icon: None,
            actions: vec![Action {
                id: ActionId::new("open").unwrap(),
                kind: ActionKind::Open,
                label: "Open".into(),
            }],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "test".into(),
        }
    }

    fn pinned(kind: Kind, id: &str, title: &str) -> Item {
        Item {
            append_to_end: true,
            ..item(kind, id, title)
        }
    }

    // --- Named directly in the brief. ---

    #[test]
    fn exclusive_mode_filters_to_kind() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::Window, "window:1", "Firefox"),
            item(Kind::App, "app:firefox", "Firefox"),
        ];
        let out = pipeline.assemble("w fire", checked(items), 10).items;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::Window);
    }

    // NOTE ON TEST DATA: the brief's illustrative example — "2+2" over a
    // calculator item and an app titled "2048" — doesn't survive contact
    // with the actual ranker built in M1.4. Nucleo's fuzzy matcher requires
    // every needle character to appear, in order, in the haystack (the same
    // property `rank.rs::tests::one_character_substitution_typo_is_not_recovered`
    // documents); "2048" contains no `+` at all, so `Ranker::rank` would
    // drop it as a non-match *before* step 7 ever ran — no promotion logic
    // could resurrect an item the ranker already filtered out, and that
    // would be true no matter how step 7 is written. Confirmed empirically
    // against `nucleo_matcher` directly: `Pattern::new("2+2", ...,
    // AtomKind::Fuzzy).score(Utf32Str::new("2048", ...), ...)` returns
    // `None`. (This comment said `Pattern::parse` until the ranker stopped
    // parsing its term as a query DSL — see the "matched literally" section
    // of `rank.rs`'s module docs. The conclusion is unchanged either way:
    // `+` is not one of the four sigils the two constructors disagree about,
    // so "2048" fails to match for the same reason under both.)
    //
    // This test keeps the exact mechanism the acceptance criterion is
    // actually about — promotion reorders without removing an item that
    // legitimately ranks — using an app title that does fuzzy-match the
    // term, so the app survives step 6 on its own merits and this test can
    // isolate what step 7 does.
    #[test]
    fn inferred_utility_pins_on_top_without_hiding_others() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            // App (weight 20) would rank above Calculator (weight 6) on
            // fuzzy score alone: both titles match "2+2" as a clean prefix,
            // so weight is what decides the unpromoted order.
            item(Kind::App, "app:puzzle", "2+2 Puzzle"),
            item(Kind::Calculator, "calc:2+2", "2+2 = 4"),
        ];
        let out = pipeline.assemble("2+2", checked(items), 10).items;
        assert_eq!(
            out[0].kind,
            Kind::Calculator,
            "the inferred utility result must lead, even though App outweighs Calculator"
        );
        assert!(
            out.iter().any(|i| i.kind == Kind::App),
            "the audited fix: promoting the calculator result must not hide the app"
        );
        assert_eq!(out.len(), 2, "nothing should be dropped by the promotion");
    }

    #[test]
    fn append_to_end_items_come_last_regardless_of_score() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            pinned(Kind::WebSearch, "web:search", "Search the web for firefox"),
            item(Kind::App, "app:firefox", "Firefox"),
        ];
        let out = pipeline.assemble("firefox", checked(items), 10).items;
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].kind,
            Kind::App,
            "the ranked app must come first even though WebSearch (25) outweighs App (20)"
        );
        assert_eq!(out[1].id, ItemId::new("web:search").unwrap());
    }

    #[test]
    fn alias_rewrite_changes_ranking_term() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"ff","type":"rewrite","target":{"query":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox"),
            item(Kind::App, "app:files", "Files"),
        ];
        let out = pipeline.assemble("ff", checked(items), 10).items;
        assert_eq!(
            out.len(),
            1,
            "ranking must behave as if \"firefox\" had been typed"
        );
        assert_eq!(out[0].title, "Firefox");
    }

    /// The second, non-interactive sink for the same bug the ranker fixes:
    /// step 6 substitutes `alias_effect.effective_term` into the query the
    /// ranker sees, so a rewrite target reaches the ranker as text the *user
    /// never typed* and cannot proofread. While the ranker parsed its term
    /// as a query DSL, an alias whose target began with `!` silently
    /// inverted matching — `nf` here would have returned every item except
    /// the ones matching "firefox", which is both wrong and impossible to
    /// diagnose from the alias config alone. The effective term is matched
    /// literally now, so the target means the eight characters it spells.
    #[test]
    fn an_alias_rewriting_to_a_leading_bang_does_not_invert_matching() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"nf","type":"rewrite","target":{"query":"!firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox"),
            item(Kind::App, "app:files", "Files"),
            item(Kind::Action, "action:bug", "!firefox crash note"),
        ];
        let out = pipeline.assemble("nf", checked(items), 10).items;
        let titles: Vec<_> = out.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["!firefox crash note"],
            "the rewrite target must match literally; inverted matching would \
             have returned \"Files\" — everything *but* the firefox items"
        );
    }

    #[test]
    fn learning_boost_applied_and_beaten_by_alias() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"winner"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        for _ in 0..10 {
            pipeline
                .learning
                .record_launch("fire", &ItemId::new("app:learned").unwrap());
        }
        let items = vec![
            item(Kind::App, "app:learned", "Fireplace"),
            item(Kind::App, "app:winner", "Fire Alarm"),
        ];

        // Sanity check: learning alone moves "app:learned" ahead of an
        // otherwise-equal competitor.
        let mut unaliased_pipeline = Pipeline::default();
        for _ in 0..10 {
            unaliased_pipeline
                .learning
                .record_launch("fire", &ItemId::new("app:learned").unwrap());
        }
        let sanity = unaliased_pipeline
            .assemble("fire", checked(items.clone()), 10)
            .items;
        assert_eq!(
            sanity[0].id,
            ItemId::new("app:learned").unwrap(),
            "learning boost alone should move its item to the front"
        );

        // The competing `ALIAS_BOOST` beats the learning boost (capped at
        // `LEARNING_BOOST_CAP`) on the other item. The alias targets
        // `app:winner`, which the `"fire" -> {"appId":"winner"}` alias means
        // as the apps provider's item — so, unlike the sanity check above,
        // this item must actually come from that provider for the boost to
        // land.
        let assembly = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    "test",
                    ALL_KINDS.to_vec(),
                    vec![item(Kind::App, "app:learned", "Fireplace")],
                ),
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:winner", "Fire Alarm")
                    }],
                ),
            ]),
            10,
        );
        // Restores the guard `checked()` gives every other test in this
        // file for free: without it, this is an ordering test that could
        // quietly become a rejection test instead (e.g. if a future change
        // to the manifest/provider wiring above started rejecting
        // "app:winner", the assertion below on a *shorter* `out` could still
        // find `out[0]` equal to itself trivially wrong in a way this guard
        // catches immediately).
        assert!(
            assembly.rejections.is_empty(),
            "both providers here are self-consistent; neither should be rejected"
        );
        assert_eq!(
            assembly.items[0].id,
            ItemId::new("app:winner").unwrap(),
            "an alias boost on a competing item must still win over learning"
        );
    }

    #[test]
    fn max_results_cap_counts_pinned_tail() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:a", "Alpha"),
            item(Kind::App, "app:b", "Bravo"),
            pinned(Kind::WebSearch, "web:search", "Search the web"),
        ];
        let out = pipeline.assemble("", checked(items), 3).items;
        assert_eq!(
            out.len(),
            3,
            "cap of 3 over 2 ranked + 1 pinned yields 3, not 4"
        );
        assert_eq!(
            out[2].id,
            ItemId::new("web:search").unwrap(),
            "the pinned item stays last"
        );
    }

    // --- Not named in the brief, but required by it. ---

    // What this test does and does not establish: with today's
    // one-kind-per-mode table (see `kinds_for_mode`), step 5's exclusive
    // filter always leaves `body` homogeneous — every survivor is already
    // the one kind the mode serves — so re-running `promote_kinds` on it in
    // step 7 would be a structural no-op regardless of the `!exclusive`
    // guard. Deleting that guard entirely would not make this test fail.
    // What this test *does* pin is the observable behavior of an explicit
    // prefix: exactly the mode's kind comes back, nothing else. The guard
    // itself is still correct to keep, because the mapping is not
    // guaranteed to stay one-kind-per-mode — if a mode is ever widened to
    // serve several kinds, an exclusive query's `body` would become
    // heterogeneous after step 5, and running step 7's promotion again on
    // it would then be observable (and wrong, since step 5 already ordered
    // it exactly as the user asked). `tests::promote_kinds_is_a_stable_reorder`
    // below pins the promotion helper's own behavior directly, independent
    // of whether any caller's guard is present.
    #[test]
    fn explicit_prefix_does_not_trigger_step_seven_promotion() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:terminal", "Terminal"),
            item(Kind::Window, "window:terminal", "Terminal"),
        ];
        let out = pipeline.assemble("w terminal", checked(items), 10).items;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::Window);
    }

    // Direct coverage of `promote_kinds` itself, independent of `assemble`
    // and its `!exclusive` guard (see the comment on
    // `explicit_prefix_does_not_trigger_step_seven_promotion` above for why
    // that guard's absence is not currently detectable through `assemble`).
    #[test]
    fn promote_kinds_is_a_no_op_on_a_homogeneous_list() {
        let mut items = vec![
            item(Kind::Window, "window:1", "Alpha"),
            item(Kind::Window, "window:2", "Bravo"),
        ];
        let before: Vec<_> = items.iter().map(|i| i.id.clone()).collect();
        promote_kinds(&mut items, &[Kind::Window]);
        let after: Vec<_> = items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(before, after, "nothing to promote, nothing to reorder");
    }

    #[test]
    fn promote_kinds_stably_reorders_a_heterogeneous_list() {
        let mut items = vec![
            item(Kind::File, "file:1", "Alpha"),
            item(Kind::Calculator, "calc:1", "Bravo"),
            item(Kind::App, "app:1", "Charlie"),
            item(Kind::Calculator, "calc:2", "Delta"),
            item(Kind::File, "file:2", "Echo"),
        ];
        promote_kinds(&mut items, &[Kind::Calculator]);
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["calc:1", "calc:2", "file:1", "app:1", "file:2"],
            "promoted kind leads, relative order preserved within both \
             groups, nothing dropped"
        );
    }

    #[test]
    fn mode_all_neither_filters_nor_promotes() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox"),
            item(Kind::File, "file:firefox", "firefox.txt"),
        ];
        let out = pipeline.assemble("firefox", checked(items), 10).items;
        assert_eq!(out.len(), 2, "Mode::All must not filter anything out");
    }

    #[test]
    fn empty_term_returns_everything_ordered_by_weight_and_boost_with_tail_last() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::File, "file:a", "Alpha"),
            item(Kind::App, "app:b", "Bravo"),
            item(Kind::Window, "window:c", "Charlie"),
            pinned(Kind::WebSearch, "web:search", "Search the web"),
        ];
        let out = pipeline.assemble("", checked(items), 10).items;
        let titles: Vec<_> = out.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Charlie", "Bravo", "Alpha", "Search the web"],
            "window > app > file by weight, with the pinned tail still last"
        );
    }

    // Deliberate choice, pinned per the brief: step 4 splits the flagged
    // items off *before* step 5's exclusive-mode filter runs, so the filter
    // never sees one. A pinned item therefore survives an exclusive filter
    // even when its own kind doesn't match the mode the user asked for. The
    // pin budget is the only thing that can refuse it, and the single pin
    // here is well inside both halves of it.
    #[test]
    fn pinned_item_survives_exclusive_filter_regardless_of_kind() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::Window, "window:1", "Firefox"),
            pinned(Kind::WebSearch, "web:search", "Search the web for firefox"),
        ];
        let out = pipeline.assemble("w fire", checked(items), 10).items;
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.last().unwrap().id,
            ItemId::new("web:search").unwrap(),
            "the pinned WebSearch item survives a Windows-exclusive filter \
             because step 4 already removed it from consideration by step 5"
        );
    }

    // Pins the truncation decision documented above the `truncate` call in
    // `assemble`: a ranked body that alone reaches `max_results` squeezes
    // the pinned tail out entirely, rather than the cap reserving room for
    // it.
    #[test]
    fn max_results_cap_squeezes_out_the_pinned_tail_when_the_ranked_body_alone_fills_it() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:a", "Alpha"),
            item(Kind::App, "app:b", "Bravo"),
            pinned(Kind::WebSearch, "web:search", "Search the web"),
        ];
        let out = pipeline.assemble("", checked(items), 2).items;
        assert_eq!(out.len(), 2);
        assert!(
            out.iter().all(|i| i.kind != Kind::WebSearch),
            "the pinned tail is squeezed out entirely once the ranked body alone fills the cap"
        );
    }

    /// Convenience for the tests below: the ids of the assembled items, which
    /// is what "never appears in the assembled output" is asserted against.
    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    // --- The pin budget: one pin per provider, three per query. ---
    //
    // Every test here passes a `max_results` far above anything it asserts on,
    // so what shortens these lists is demonstrably the pin budget and not step
    // 9's cap.

    /// One provider's flood: `count` items, every one flagged, all titled so
    /// that nothing these tests type matches them — which is the point, since
    /// a pinned item is placed without having matched anything. The ids count
    /// from zero in the order the provider returns them, which is the order
    /// the pin budget spends.
    fn flood(provider_id: &str, count: usize) -> Vec<Item> {
        (0..count)
            .map(|n| Item {
                provider: provider_id.into(),
                ..pinned(
                    Kind::WebSearch,
                    &format!("{provider_id}:{n}"),
                    "Search the web",
                )
            })
            .collect()
    }

    /// One provider's flood, paired with that provider's own manifest —
    /// [`flood`] built into the [`ProviderOutput`] the pin budget sees.
    fn flood_output(provider_id: &'static str, count: usize) -> ProviderOutput {
        output(
            provider_id,
            vec![Kind::WebSearch],
            flood(provider_id, count),
        )
    }

    /// One provider's single pinned row, paired with that provider's own
    /// manifest — for the tests that need two outputs to name the same
    /// producer, which [`flood_output`]'s generated ids cannot do without
    /// colliding.
    fn pin_output(provider_id: &'static str, id: &str) -> ProviderOutput {
        output(
            provider_id,
            vec![Kind::WebSearch],
            vec![Item {
                provider: provider_id.into(),
                ..pinned(Kind::WebSearch, id, "Search the web")
            }],
        )
    }

    /// The ids of a rejection list, for asserting *which* items were declined.
    fn ids_of_rejections(rejections: &[Rejection]) -> Vec<String> {
        rejections
            .iter()
            .map(|r| r.item_id.to_string())
            .collect::<Vec<_>>()
    }

    #[test]
    fn a_provider_flooding_the_pinned_tail_is_honored_for_one_pin_only() {
        let mut pipeline = Pipeline::default();
        let out = pipeline
            .assemble(
                "firefox",
                CheckedItems::check(vec![flood_output("web", 8)]),
                50,
            )
            .items;
        assert_eq!(
            ids(&out),
            vec!["web:0"],
            "a provider that sets append_to_end on everything it returns gets \
             MAX_PINNED_ITEMS_PER_PROVIDER pins, however many it sends: without \
             the pin budget all 8 would be here, none of them scored against \
             \"firefox\" or filtered by anything. The one it keeps is the first \
             it returned, because provider-supplied order is the only order the \
             pinned tail has — it is never scored — so a provider that reorders \
             its own output chooses which of its own pins survives"
        );
    }

    #[test]
    fn one_provider_cannot_take_a_second_pin_while_the_query_total_is_unspent() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![flood_output("web", 2)]),
            50,
        );
        assert_eq!(
            ids(&out.items),
            vec!["web:0"],
            "two pins from one provider, and a query total of 3 nothing else is \
             competing for: the second is still refused, because the per-provider \
             share is a cap in its own right and not merely a way of dividing up \
             a contested total"
        );
        assert_eq!(
            ids_of_rejections(&out.rejections),
            vec!["web:1"],
            "and the refused pin is reported rather than dropped in silence"
        );
    }

    #[test]
    fn three_different_providers_can_each_land_one_pinned_row() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                flood_output("web", 1),
                flood_output("actions", 1),
                flood_output("files", 1),
            ]),
            50,
        );
        assert_eq!(
            ids(&out.items),
            vec!["web:0", "actions:0", "files:0"],
            "the per-provider share must not collapse into a total of one: three \
             providers asking for one pin each are all honored, in the order \
             their outputs were checked"
        );
        assert!(
            out.rejections.is_empty(),
            "nothing here exceeds either half of the pin budget"
        );
    }

    /// The share is counted over producer *ids* against the tail being built,
    /// not over [`ProviderOutput`] values, and one provider may answer a
    /// single query with more than one output — a scheduler streaming partial
    /// results is the obvious way to get there. So the share has to survive
    /// being split across them. Nothing else in this module would notice a
    /// refactor that counted one pin per output instead: every other test here
    /// gives each producer exactly one output, so all of them would still pass
    /// while this provider quietly took two rows.
    #[test]
    fn a_producer_answering_in_two_outputs_still_gets_one_pin() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                pin_output("web", "web:first"),
                pin_output("web", "web:second"),
            ]),
            50,
        );
        assert_eq!(
            ids(&out.items),
            vec!["web:first"],
            "one producer, two outputs, one pin: the share belongs to the \
             provider that produced the items, not to the output it arrived in"
        );
        assert_eq!(
            ids_of_rejections(&out.rejections),
            vec!["web:second"],
            "and the second output's pin is refused for being the producer's \
             second, exactly as it would have been inside one output"
        );
    }

    /// Several providers flooding at once, which the tests above do not cover:
    /// each is held to its own row rather than the first flooder taking the
    /// query total, and the total is reached by three *different* producers
    /// having spent it — not by any one of them.
    #[test]
    fn several_flooding_providers_are_each_held_to_one_pin() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                flood_output("first", 4),
                flood_output("second", 4),
                flood_output("third", 4),
            ]),
            50,
        );
        assert_eq!(
            ids(&out.items),
            vec!["first:0", "second:0", "third:0"],
            "twelve pins requested by three providers yields one row each, in \
             the order their outputs were checked"
        );
        assert_eq!(
            out.rejections.len(),
            9,
            "and the other nine are refused: three per provider, none of them \
             for anything the provider claimed about itself"
        );
        assert!(
            out.rejections
                .iter()
                .all(|r| r.check == FailedCheck::PinBudget),
        );
    }

    #[test]
    fn a_fourth_provider_is_refused_once_the_query_total_is_spent() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                flood_output("web", 1),
                flood_output("actions", 1),
                flood_output("files", 1),
                flood_output("fourth", 1),
            ]),
            50,
        );
        assert_eq!(
            out.items.len(),
            MAX_PINNED_ITEMS_PER_QUERY,
            "the per-provider share does not make the query total decorative: a \
             fourth provider within its own share is still refused once the \
             query has spent MAX_PINNED_ITEMS_PER_QUERY pins"
        );
        assert_eq!(ids_of_rejections(&out.rejections), vec!["fourth:0"]);
    }

    /// The regression that decided the per-provider share, now pinned as the
    /// guarantee it produces. A flooding provider whose output is checked
    /// first used to spend the whole query total and leave the genuine
    /// web-search row rejected — placed before this issue's change, dropped
    /// after it, which is a regression against "the existing first-party
    /// pinned-row behavior is unchanged". Sharing the budget by producer fixes
    /// it without any notion of who is first-party: the flooder is refused a
    /// second pin because it already has one, not because of who it is.
    #[test]
    fn a_flooding_provider_that_answers_first_cannot_crowd_out_another_providers_pin() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                flood_output("evil", 8),
                output(
                    "web",
                    vec![Kind::WebSearch],
                    vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:search", "Search the web")
                    }],
                ),
            ]),
            50,
        );
        assert_eq!(
            ids(&out.items),
            vec!["evil:0", "web:search"],
            "the flooder answered first and still takes exactly one slot, so the \
             first-party row lands: what it is refused is a second pin, which \
             needs no notion of who may pin at all"
        );
        assert!(
            out.rejections
                .iter()
                .all(|r| r.check == FailedCheck::PinBudget),
            "the flooder is honest about its own output — its items fail no \
             manifest check — and the seven it does not get back are pin-budget \
             refusals"
        );
    }

    #[test]
    fn pinned_items_past_the_pin_budget_come_back_as_rejections() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![flood_output("web", 4)]),
            50,
        );
        assert_eq!(
            ids_of_rejections(&out.rejections),
            vec!["web:1", "web:2", "web:3"],
            "the refusal is observable: every pinned item the budget would not \
             honor comes back as a Rejection, in the order the providers \
             returned them, rather than disappearing between step 4 and the \
             returned list"
        );
        assert!(
            out.rejections
                .iter()
                .all(|r| r.check == FailedCheck::PinBudget),
            "this provider is honest about its own output; the only thing wrong \
             with these items is that its share of the pin budget was spent"
        );
    }

    /// The whole record for a pin-budget refusal, field by field — the
    /// companion to
    /// `tests::a_rejection_names_the_item_the_claim_the_producer_and_the_failed_check`
    /// for the one [`FailedCheck`] that is not a manifest check. Both provider
    /// fields hold the producer's id, and what the manifest checks bought is
    /// that the id is the producer's real one rather than a claim.
    #[test]
    fn a_pin_budget_rejection_names_the_item_and_the_provider_that_produced_it() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![flood_output("web", 2)]),
            50,
        );
        assert_eq!(
            out.rejections,
            vec![Rejection {
                item_id: ItemId::new("web:1").unwrap(),
                claimed_kind: Kind::WebSearch,
                claimed_provider: "web".into(),
                producer_id: "web".into(),
                check: FailedCheck::PinBudget,
            }]
        );
    }

    /// The intended first-party use, unchanged by the pin budget: one pinned
    /// row, honored, through an exclusive filter its kind does not match — the
    /// same case `tests::pinned_item_survives_exclusive_filter_regardless_of_kind`
    /// pins, with the addition that staying inside the budget costs nothing.
    #[test]
    fn a_single_first_party_pinned_row_is_honored_and_rejected_by_nothing() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::Window, "window:1", "Firefox"),
            pinned(Kind::WebSearch, "web:search", "Search the web for firefox"),
        ];
        let out = pipeline.assemble("w fire", checked(items), 10);
        assert_eq!(
            ids(&out.items),
            vec!["window:1", "web:search"],
            "the one pinned row the flag exists for is placed exactly as before"
        );
        assert!(
            out.rejections.is_empty(),
            "a query within the pin budget rejects nothing: the budget is a \
             ceiling on the pinned path, not a toll on using it"
        );
    }

    // --- The two manifest checks, and the three abuses they close. ---

    #[test]
    fn item_whose_kind_is_outside_its_producers_declared_kinds_is_rejected() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            Item {
                provider: "calc".into(),
                ..item(Kind::Calculator, "calc:2+2", "2+2 = 4")
            },
            Item {
                provider: "calc".into(),
                ..item(Kind::Window, "window:1", "Firefox")
            },
        ];
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![output("calc", vec![Kind::Calculator], items)]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["calc:2+2"],
            "a provider declaring kinds: [Calculator] cannot also emit a Window item"
        );
        assert_eq!(out.rejections.len(), 1);
        assert_eq!(out.rejections[0].check, FailedCheck::Kind);
    }

    #[test]
    fn item_whose_provider_string_does_not_match_its_producer_is_rejected() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            Item {
                provider: APPS_PROVIDER_ID.into(),
                ..item(Kind::App, "app:files", "Files")
            },
            Item {
                provider: "not-the-apps-provider".into(),
                ..item(Kind::App, "app:firefox", "Firefox")
            },
        ];
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![output(APPS_PROVIDER_ID, vec![Kind::App], items)]),
            10,
        );
        assert_eq!(ids(&out.items), vec!["app:files"]);
        assert_eq!(out.rejections.len(), 1);
        assert_eq!(out.rejections[0].check, FailedCheck::Provenance);
    }

    /// Abuse 1 — boost theft. The alias `fire` boosts item id `app:firefox`
    /// by [`crate::aliases::ALIAS_BOOST`], far more than any fuzzy score
    /// separates these two titles by, so an impostor carrying that id leads
    /// the list if it survives at all.
    #[test]
    fn a_rejected_item_collects_no_boost() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let out = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:fireplace", "Fireplace")
                    }],
                ),
                // Produced by `evil`, but claiming to be the apps provider's
                // work — the forged item from the issue.
                output(
                    "evil",
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox Impostor")
                    }],
                ),
            ]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["app:fireplace"],
            "the impostor must not appear at all, let alone lead on an alias \
             boost keyed to the id it forged"
        );
    }

    // --- Task 2: alias boosts scoped to their target provider. ---
    //
    // A gap the two manifest checks above cannot close on their own: a
    // provider that declares itself *honestly* — `id: "evil"`, its own
    // `kinds` — can still emit an item whose id collides with the apps
    // provider's namespace an `AppBoost` alias targets. That item passes
    // both manifest checks cleanly (its `provider` field agrees with its
    // own producer), so it survives into `CheckedItems::items()` right
    // alongside the genuine apps-provider item sharing its id. Only
    // `Boosts::by_provider_item`'s provider dimension (via
    // `AliasEffect::boosts` tagging every `AppBoost` with
    // [`APPS_PROVIDER_ID`]) tells the two apart at scoring time.

    /// The acceptance case this scoping exists for: an alias boost
    /// configured for the apps provider must not land on an identically-id'd
    /// item a different, honestly self-declared provider produced.
    #[test]
    fn alias_boost_does_not_land_on_an_identically_id_item_from_a_different_provider() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let out = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                ),
                // Honestly declares itself as a Window provider — no
                // impersonation, so this item passes both manifest checks —
                // but happens to reuse the id "app:firefox" the alias above
                // targets.
                output(
                    "windows",
                    vec![Kind::Window],
                    vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "app:firefox", "Firefox")
                    }],
                ),
            ]),
            10,
        );
        assert!(
            out.rejections.is_empty(),
            "both providers are honest about their own output; neither should be rejected"
        );
        assert_eq!(
            out.items[0].kind,
            Kind::App,
            "without the fix, the boost keyed only to the id would also lift \
             the Window item — weight 30 to App's 20 — and it would stay on \
             top despite not being who the alias actually targets"
        );
    }

    /// The other half: the fix must not stop the boost from landing on the
    /// item it is actually for. Same shape as
    /// `rank::tests::boost_applies_to_the_right_item`, run through the full
    /// pipeline with the apps provider now spelled out explicitly, so the
    /// resulting order is provably unchanged from before this change scoped
    /// the boost to a provider.
    #[test]
    fn alias_boost_still_lands_on_the_genuine_apps_item_same_order_as_before() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        // Without the boost, Window (weight 30) would outrank App (weight
        // 20) on this tie — `ALIAS_BOOST` must still flip it.
        let assembly = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                ),
                output(
                    "windows",
                    vec![Kind::Window],
                    vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "window:1", "Firefox")
                    }],
                ),
            ]),
            10,
        );
        assert!(
            assembly.rejections.is_empty(),
            "both providers are self-consistent; neither should be rejected"
        );
        // The full order, not just who's first: an assertion on `out[0]`
        // alone would still pass if the Window item vanished entirely
        // (dropped by a future exclusive-filter change, a CheckedItems
        // regression, ...) without the alias boost ever applying — proving
        // nothing about the boost. Asserting both positions is what actually
        // pins "the same resulting order as before this change".
        assert_eq!(
            ids(&assembly.items),
            vec!["app:firefox", "window:1"],
            "the genuine apps-provider item still receives its alias boost \
             and outranks the higher-weighted Window item, exactly as it did \
             before the boost was scoped to a provider"
        );
    }

    /// Abuse 2 — eviction. `Ranker::rank` dedupes apps on **title alone**
    /// (see `rank::tests::duplicate_apps_deduped_by_title`), keeping the
    /// best-scoring occurrence, so an impostor sharing the genuine Firefox's
    /// title and outscoring it on a learning boost silently deletes the
    /// genuine item from the list.
    #[test]
    fn a_rejected_item_cannot_evict_a_genuine_item_through_dedupe() {
        let mut pipeline = Pipeline::default();
        for _ in 0..10 {
            pipeline
                .learning
                .record_launch("firefox", &ItemId::new("app:evil").unwrap());
        }
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                ),
                output(
                    "evil",
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:evil", "Firefox")
                    }],
                ),
            ]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["app:firefox"],
            "the genuine item must survive: the impostor it shares a title \
             with was rejected before dedupe could prefer the impostor"
        );
    }

    /// Abuse 3 — exclusive-mode bypass. A provider declaring `kinds:
    /// [Calculator]` returns a `Kind::Window` item, which without the kind
    /// check passes step 5's `w `-exclusive filter and inherits Window's
    /// ranking weight.
    #[test]
    fn a_rejected_item_cannot_survive_an_exclusive_mode_filter() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "w fire",
            CheckedItems::check(vec![
                output(
                    "windows",
                    vec![Kind::Window],
                    vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "window:1", "Firefox")
                    }],
                ),
                output(
                    "calc",
                    vec![Kind::Calculator],
                    vec![Item {
                        provider: "calc".into(),
                        ..item(Kind::Window, "window:evil", "Firefox Impostor")
                    }],
                ),
            ]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["window:1"],
            "only the provider that declared Kind::Window may answer a \
             Windows-exclusive query"
        );
        assert_eq!(out.rejections[0].check, FailedCheck::Kind);
    }

    /// The flagged items are split off before step 5 and never ranked, so the
    /// pinned tail is the one path into the output that no later step filters
    /// — an unchecked pinned item would be a hole straight through this work.
    /// The manifest checks run before the pin budget is spent, so this holds
    /// for a flagged item whether or not the budget goes on to honor it. The
    /// query here is `w `-exclusive precisely because that is the filter a
    /// pinned item legitimately bypasses.
    #[test]
    fn a_rejected_append_to_end_item_is_rejected_too() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "w fire",
            CheckedItems::check(vec![
                output(
                    "web",
                    vec![Kind::WebSearch],
                    vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:search", "Search the web for firefox")
                    }],
                ),
                output(
                    "evil",
                    vec![Kind::WebSearch],
                    vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:evil", "Search the web, evilly")
                    }],
                ),
            ]),
            10,
        );
        assert_eq!(ids(&out.items), vec!["web:search"]);
        assert_eq!(out.rejections.len(), 1);
        assert_eq!(out.rejections[0].item_id, ItemId::new("web:evil").unwrap());
        assert_eq!(out.rejections[0].check, FailedCheck::Provenance);
    }

    /// The whole rejection record, field by field — this is what a future
    /// logging seam gets to work with. The item here fails *both* checks
    /// (wrong kind for its producer, and a forged provider string), which
    /// pins the DECISION on [`CheckedItems::check`]: one rejection per
    /// rejected item, reported against the kind check.
    #[test]
    fn a_rejection_names_the_item_the_claim_the_producer_and_the_failed_check() {
        let mut pipeline = Pipeline::default();
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![output(
                "evil",
                vec![Kind::Calculator],
                vec![Item {
                    provider: APPS_PROVIDER_ID.into(),
                    ..item(Kind::Window, "app:firefox", "Firefox")
                }],
            )]),
            10,
        );
        assert!(out.items.is_empty());
        assert_eq!(
            out.rejections,
            vec![Rejection {
                item_id: ItemId::new("app:firefox").unwrap(),
                claimed_kind: Kind::Window,
                claimed_provider: APPS_PROVIDER_ID.into(),
                producer_id: "evil".into(),
                check: FailedCheck::Kind,
            }]
        );
    }

    /// Each item is checked against *its own* producer's manifest, never
    /// against the union of every manifest that answered. Both impostors here
    /// are well-behaved by the union's standards and rejected by their own
    /// producer's: `apps` emits a Calculator item, a kind `calc` (also
    /// answering) declares; `calc` emits an item claiming provider `apps`, an
    /// id `apps` (also answering) really has. An implementation that hoisted
    /// the declared kinds or the ids into one set spanning `outputs` — an
    /// easy thing to reach for with many providers — would let both through
    /// while keeping every other test in this module green.
    #[test]
    fn an_item_is_checked_against_its_own_producer_not_the_union_of_every_manifest() {
        let checked = CheckedItems::check(vec![
            output(
                APPS_PROVIDER_ID,
                vec![Kind::App],
                vec![
                    Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    },
                    Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::Calculator, "calc:evil", "2+2 = 5")
                    },
                ],
            ),
            output(
                "calc",
                vec![Kind::Calculator],
                vec![
                    Item {
                        provider: "calc".into(),
                        ..item(Kind::Calculator, "calc:2+2", "2+2 = 4")
                    },
                    Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::Calculator, "calc:impostor", "2+2 = 6")
                    },
                ],
            ),
        ]);

        assert_eq!(
            ids(checked.items()),
            vec!["app:firefox", "calc:2+2"],
            "only each provider's own honest items survive, in the order the \
             providers returned them"
        );
        assert_eq!(
            checked.rejections(),
            vec![
                Rejection {
                    item_id: ItemId::new("calc:evil").unwrap(),
                    claimed_kind: Kind::Calculator,
                    claimed_provider: APPS_PROVIDER_ID.into(),
                    producer_id: APPS_PROVIDER_ID.into(),
                    check: FailedCheck::Kind,
                },
                Rejection {
                    item_id: ItemId::new("calc:impostor").unwrap(),
                    claimed_kind: Kind::Calculator,
                    claimed_provider: APPS_PROVIDER_ID.into(),
                    producer_id: "calc".into(),
                    check: FailedCheck::Provenance,
                },
            ],
            "a kind another answering provider declares does not vouch for \
             this one's item, and neither does another answering provider's id"
        );
    }

    /// The association is only worth anything if it is the *right* manifest:
    /// [`ProviderOutput::from_provider`] must take it from the provider it is
    /// handed, not from anywhere the caller could substitute. Pairing the
    /// same items with a different provider rejects every one of them, which
    /// is what makes the pairing load-bearing rather than decorative.
    #[test]
    fn from_provider_takes_the_manifest_from_the_provider_it_is_given() {
        let items = vec![Item {
            provider: APPS_PROVIDER_ID.into(),
            ..item(Kind::App, "app:firefox", "Firefox")
        }];

        let own = CheckedItems::check(vec![ProviderOutput::from_provider(
            &provider(APPS_PROVIDER_ID, vec![Kind::App]),
            items.clone(),
        )]);
        assert!(own.rejections().is_empty());
        assert_eq!(ids(own.items()), vec!["app:firefox"]);

        let someone_elses = CheckedItems::check(vec![ProviderOutput::from_provider(
            &provider("windows", vec![Kind::App]),
            items,
        )]);
        assert!(someone_elses.items().is_empty());
        assert_eq!(
            someone_elses.rejections()[0].producer_id,
            "windows",
            "the manifest checked against is the one the given provider \
             describes itself with"
        );
        assert_eq!(someone_elses.rejections()[0].check, FailedCheck::Provenance);
    }

    // --- Task 1 (issue #103): the accumulator's merge and cap. ---

    /// Two already-checked values, each carrying one surviving item and one
    /// rejection of its own, merge with both lists preserved in order —
    /// `self`'s first, then `other`'s. This is the shape an accumulator
    /// actually needs: a query's providers answer one at a time, each batch
    /// arrives already checked against its own producer, and `absorb` is
    /// what turns a run of those into the one whole-query value
    /// [`Pipeline::assemble`] takes.
    #[test]
    fn absorb_concatenates_items_and_rejections_in_order() {
        let mut first = CheckedItems::check(vec![output(
            "one",
            vec![Kind::App],
            vec![
                Item {
                    provider: "one".into(),
                    ..item(Kind::App, "app:a", "Alpha")
                },
                // Wrong kind for its own producer's manifest — a rejection
                // belonging to `first`.
                Item {
                    provider: "one".into(),
                    ..item(Kind::Window, "window:bad", "Bad")
                },
            ],
        )]);
        let second = CheckedItems::check(vec![output(
            "two",
            vec![Kind::App],
            vec![
                Item {
                    provider: "two".into(),
                    ..item(Kind::App, "app:b", "Bravo")
                },
                // Forged provenance — a rejection belonging to `second`.
                Item {
                    provider: "someone-else".into(),
                    ..item(Kind::App, "app:forged", "Forged")
                },
            ],
        )]);

        first.absorb(second);

        assert_eq!(
            ids(first.items()),
            vec!["app:a", "app:b"],
            "both sides' surviving items land in order, self's first"
        );
        assert_eq!(
            ids_of_rejections(first.rejections()),
            vec!["window:bad", "app:forged"],
            "and so do both sides' rejections, in the same self-then-other order"
        );
    }

    /// Keeps the first `n` items and drops the rest — leaving rejections
    /// alone, since a rejection was never a candidate for this cap: it
    /// already recorded why a manifest check declined its item, and that
    /// record has nothing to do with how many *surviving* items the caller
    /// wants kept.
    ///
    /// Three rejections against a cap of two, deliberately more rejections
    /// than the `max` this call passes: an implementation that truncated
    /// both `Vec`s to `max` — plausible if `truncate_items` were written by
    /// analogy to a single cap applied everywhere — would leave two
    /// rejections here, not three, and this is the number that catches it.
    /// One rejection surviving a truncation to two would not: it sits below
    /// `max` either way, so a bug that also caps rejections at `max` could
    /// hide behind it.
    #[test]
    fn truncate_items_keeps_the_first_n_and_leaves_rejections_alone() {
        let mut checked = CheckedItems::check(vec![output(
            "test",
            vec![Kind::App, Kind::Window],
            vec![
                item(Kind::App, "app:a", "Alpha"),
                item(Kind::App, "app:b", "Bravo"),
                item(Kind::App, "app:c", "Charlie"),
                // Calculator is not among this producer's declared kinds —
                // three rejections, present both before and after
                // truncation.
                item(Kind::Calculator, "calc:bad1", "Bad 1"),
                item(Kind::Calculator, "calc:bad2", "Bad 2"),
                item(Kind::Calculator, "calc:bad3", "Bad 3"),
            ],
        )]);
        assert_eq!(
            checked.rejections().len(),
            3,
            "sanity: three rejections going in"
        );

        checked.truncate_items(2);

        assert_eq!(
            ids(checked.items()),
            vec!["app:a", "app:b"],
            "keeps the first n, drops the rest"
        );
        assert_eq!(
            checked.rejections().len(),
            3,
            "truncating items must not touch rejections, even when there are \
             more of them than the item cap"
        );
    }

    // --- Task 2 (issue #61 / #30): provider-answer count and per-field
    // length caps in `CheckedItems::check`. ---

    /// `count` well-formed items, each short enough to pass every field
    /// check, so a test built on this is demonstrably about the *count* cap
    /// alone.
    fn many_items(count: usize) -> Vec<Item> {
        (0..count)
            .map(|n| item(Kind::App, &format!("app:{n}"), "Alpha"))
            .collect()
    }

    #[test]
    fn a_provider_answer_of_exactly_the_cap_is_unaffected() {
        let checked = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            many_items(MAX_ITEMS_PER_PROVIDER_ANSWER),
        )]);
        assert_eq!(
            checked.items().len(),
            MAX_ITEMS_PER_PROVIDER_ANSWER,
            "exactly MAX_ITEMS_PER_PROVIDER_ANSWER items must all survive"
        );
        assert!(checked.rejections().is_empty());
    }

    #[test]
    fn a_provider_answer_one_over_the_cap_drops_the_tail_silently() {
        let checked = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            many_items(MAX_ITEMS_PER_PROVIDER_ANSWER + 1),
        )]);
        assert_eq!(
            checked.items().len(),
            MAX_ITEMS_PER_PROVIDER_ANSWER,
            "the one item over the cap must not survive"
        );
        assert!(
            checked.rejections().is_empty(),
            "the excess is truncated away before a single item is inspected, \
             so it is dropped silently rather than turned into a Rejection — \
             there is nothing to reject it for"
        );
        assert_eq!(
            checked.items().last().unwrap().id.as_str(),
            format!("app:{}", MAX_ITEMS_PER_PROVIDER_ANSWER - 1),
            "the surviving items are the head of the answer, not an \
             arbitrary subset"
        );
    }

    /// An item whose `title` is exactly [`MAX_TITLE`] bytes.
    fn item_with_title(title: &str) -> Item {
        item(Kind::App, "app:title", title)
    }

    #[test]
    fn title_at_the_bound_passes_one_over_is_rejected() {
        let at_bound = checked(vec![item_with_title(&"a".repeat(MAX_TITLE))]);
        assert_eq!(
            at_bound.items().len(),
            1,
            "exactly MAX_TITLE bytes must pass"
        );

        let over = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            vec![item_with_title(&"a".repeat(MAX_TITLE + 1))],
        )]);
        assert!(over.items().is_empty());
        assert_eq!(
            over.rejections()[0].check,
            FailedCheck::FieldTooLong {
                field: "Item.title"
            }
        );
    }

    /// An item whose `subtitle` is exactly `len` bytes.
    fn item_with_subtitle(len: usize) -> Item {
        Item {
            subtitle: Some("a".repeat(len)),
            ..item(Kind::App, "app:subtitle", "Alpha")
        }
    }

    #[test]
    fn subtitle_at_the_bound_passes_one_over_is_rejected() {
        let at_bound = checked(vec![item_with_subtitle(MAX_SUBTITLE)]);
        assert_eq!(
            at_bound.items().len(),
            1,
            "exactly MAX_SUBTITLE bytes must pass"
        );

        let over = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            vec![item_with_subtitle(MAX_SUBTITLE + 1)],
        )]);
        assert!(over.items().is_empty());
        assert_eq!(
            over.rejections()[0].check,
            FailedCheck::FieldTooLong {
                field: "Item.subtitle"
            }
        );
    }

    /// An item whose `copy_text` is exactly `len` bytes.
    fn item_with_copy_text(len: usize) -> Item {
        Item {
            copy_text: Some("a".repeat(len)),
            ..item(Kind::App, "app:copy", "Alpha")
        }
    }

    #[test]
    fn copy_text_at_the_bound_passes_one_over_is_rejected() {
        let at_bound = checked(vec![item_with_copy_text(MAX_COPY_TEXT)]);
        assert_eq!(
            at_bound.items().len(),
            1,
            "exactly MAX_COPY_TEXT bytes must pass"
        );

        let over = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            vec![item_with_copy_text(MAX_COPY_TEXT + 1)],
        )]);
        assert!(over.items().is_empty());
        assert_eq!(
            over.rejections()[0].check,
            FailedCheck::FieldTooLong {
                field: "Item.copy_text"
            }
        );
    }

    /// An item with one action whose `label` is exactly `len` bytes.
    fn item_with_action_label(len: usize) -> Item {
        Item {
            actions: vec![Action {
                id: ActionId::new("open").unwrap(),
                kind: ActionKind::Open,
                label: "a".repeat(len),
            }],
            ..item(Kind::App, "app:action-label", "Alpha")
        }
    }

    #[test]
    fn action_label_at_the_bound_passes_one_over_is_rejected() {
        let at_bound = checked(vec![item_with_action_label(MAX_ACTION_LABEL)]);
        assert_eq!(
            at_bound.items().len(),
            1,
            "exactly MAX_ACTION_LABEL bytes must pass"
        );

        let over = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            vec![item_with_action_label(MAX_ACTION_LABEL + 1)],
        )]);
        assert!(over.items().is_empty());
        assert_eq!(
            over.rejections()[0].check,
            FailedCheck::FieldTooLong {
                field: "Action.label"
            }
        );
    }

    /// An item with exactly `count` actions, each well within the label
    /// bound, so a test built on this is demonstrably about the *count* of
    /// actions and not any one action's length.
    fn item_with_action_count(count: usize) -> Item {
        let actions = (0..count)
            .map(|n| Action {
                id: ActionId::new(format!("action:{n}")).unwrap(),
                kind: ActionKind::Open,
                label: "Open".into(),
            })
            .collect();
        Item {
            actions,
            ..item(Kind::App, "app:action-count", "Alpha")
        }
    }

    #[test]
    fn action_count_at_the_bound_passes_one_over_is_rejected() {
        let at_bound = checked(vec![item_with_action_count(MAX_ACTIONS_PER_ITEM)]);
        assert_eq!(
            at_bound.items().len(),
            1,
            "exactly MAX_ACTIONS_PER_ITEM actions must pass"
        );

        let over = CheckedItems::check(vec![output(
            "test",
            ALL_KINDS.to_vec(),
            vec![item_with_action_count(MAX_ACTIONS_PER_ITEM + 1)],
        )]);
        assert!(over.items().is_empty());
        assert_eq!(
            over.rejections()[0].check,
            FailedCheck::FieldTooLong {
                field: "Item.actions"
            }
        );
    }

    /// The other half of the "ranks identically" argument Task 2 adds on top
    /// of the existing suite continuing to pass unmodified: an item that
    /// fails more than one check is reported once, against whichever check
    /// runs first — here, a wrong `kind` *and* an over-long `title` on the
    /// same item. `CheckedItems::check`'s loop runs the kind check before
    /// any field-length check, so the single rejection this produces must be
    /// [`FailedCheck::Kind`], not [`FailedCheck::FieldTooLong`].
    #[test]
    fn an_item_failing_both_a_manifest_check_and_a_field_length_check_is_reported_once() {
        let mut evil = item_with_title(&"a".repeat(MAX_TITLE + 1));
        evil.kind = Kind::Window;
        evil.provider = "calc".into();

        let checked = CheckedItems::check(vec![output("calc", vec![Kind::Calculator], vec![evil])]);
        assert!(checked.items().is_empty());
        assert_eq!(
            checked.rejections().len(),
            1,
            "one failing item must be one rejection, however many checks it fails"
        );
        assert_eq!(
            checked.rejections()[0].check,
            FailedCheck::Kind,
            "the kind check runs before any field-length check, so that is \
             what the single rejection is reported against"
        );
    }
}
