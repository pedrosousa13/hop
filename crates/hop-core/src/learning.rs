//! Frecency learning: records that a query led to launching an item, and
//! later reports a boost for that pairing, so frequently-chosen results rise
//! for the queries that reach them.
//!
//! Ported (salvage, not rewritten — the audit marked this module worth
//! keeping) from the abandoned `feat/cross-linux-hopd-v1` branch's
//! `hopd::learning::LearningStore`. The differences from that source are
//! deliberate interface decisions, not fixes:
//!
//! - Renamed `LearningStore` to [`Learning`], with fully private state (the
//!   salvage exposed `version`, `selections` and `global_frequency` as
//!   `pub`).
//! - Dropped the stored `path` field: [`Learning::load`] and [`Learning::save`]
//!   both take a `&Path` explicitly instead, so nothing here remembers where
//!   it lives on disk.
//! - [`Learning::save`] takes `&self` (not `&mut self`) and returns
//!   `std::io::Result<()>` instead of swallowing every error. Since it can't
//!   mutate through `&self`, it computes the purged, canonicalized payload
//!   into a local value rather than calling `purge_expired` on `self`.
//! - [`Learning::record_launch`] and [`Learning::boost_for`] are new,
//!   `ItemId`-keyed public entry points. `boost_for` sums the salvage's
//!   `query_boost` and `frequency_boost`, clamped to
//!   `0.0..=LEARNING_BOOST_CAP` — `LEARNING_BOOST_CAP` itself is applied
//!   nowhere else. `query_boost`/`frequency_boost` keep their original
//!   `i32` scale and internal cap values (150, 60) unmodified, but each now
//!   clamps its own result to that cap rather than only bounding the upper
//!   end with `.min` — the non-negative floor used to be exclusively
//!   `boost_for`'s property, and now is not.
//! - `reset` no longer self-persists (it has no path to persist to); it only
//!   clears in-memory state now.
//! - The recorded query key is bounded (issue #22). The salvage accepted a
//!   query of any size as a `selections` key; a key over [`MAX_QUERY_TEXT`]
//!   bytes is now refused. [`Learning::record_launch`] says which caller that
//!   bound is the *only* protection for, and what it deliberately does not
//!   bound.
//! - The load path is bounded (issue #37). The salvage read the whole file
//!   with `read_to_string` — whatever the path pointed at, however large —
//!   and enforced `MAX_GLOBAL_ENTRIES` only in `record`, never on the way in.
//!   [`Learning::load`] now refuses a path that is not a regular file, reads
//!   at most `MAX_STORE_BYTES` + 1 bytes, and applies both the entry cap and
//!   the `MAX_ITEM_ID` key bound to what it parsed. Its contract is
//!   unchanged: it still returns `Learning` rather than a `Result`, still
//!   falls back to an empty state on any error, and still never panics.
//! - What a store claims about itself is no longer taken at face value
//!   (issue #38), by two different means: the version is *checked*, the
//!   timestamps are *corrected*. The salvage parsed `version` and copied it
//!   through without ever comparing it to anything, and read every `last_ms`
//!   as written — including one in the future, which disables decay
//!   outright. [`Learning::load`] now refuses a store whose version is not
//!   `STORE_VERSION`, and clamps back to the load instant those `last_ms`
//!   values that sit ahead of it, leaving every other stamp exactly as it
//!   was. `save` writes `STORE_VERSION` rather than whatever version the
//!   value in memory carries. The same three parts of `load`'s contract are
//!   again unchanged.
//! - Why a load fell back is now available (issue #43). The salvage funnelled
//!   every fallback into the same empty value and said nothing, so an absent
//!   store, one this process may not read, a damaged one and one written by a
//!   later hop were a single outcome. [`Learning::load_reporting`] returns a
//!   [`LoadReport`] beside the store, one variant per condition, and
//!   [`Learning::load`] is that function with the report dropped — so its
//!   contract is unchanged for the third time, and the load rules have one
//!   implementation rather than two that can drift. What a caller should
//!   *do* with a report is not decided here, and neither is preserving the
//!   file a report is about: `save` still overwrites it, which
//!   [`Learning::save`] says.
//! - An unrecognized id no longer reaches disk verbatim (issue #39, Decision
//!   2's shape half — superseded by the manifest half below, issue #72). The
//!   salvage — and this module, until #39 — persisted `global_frequency`'s
//!   key as whatever `canonicalize_result_id` made of the raw id, untouched
//!   for every provider but two. [`persistence_key`] replaced that as the
//!   map's key function, and originally decided plaintext versus hash by
//!   guessing at the raw id's own *shape*: three known-safe prefixes
//!   persisted in the clear, everything else — `calc:` included — hashed.
//!   That guess is gone now (the next bullet); [`persistence_key`] keeps the
//!   same job — decide the id-part before [`provider_scoped_key`] folds in
//!   the provider — but no longer looks at the id itself to do it.
//! - The shape guess is replaced by a manifest claim (issue #72, Decision 2's
//!   manifest half — the half #39 deferred here). A provider's own manifest
//!   now says, once and up front, whether its ids are safe to persist in the
//!   clear
//!   ([`ProviderManifest::ids_are_safe_to_persist_in_the_clear`](crate::provider::ProviderManifest::ids_are_safe_to_persist_in_the_clear)),
//!   and [`persistence_key`] just reads that answer instead of inferring one.
//!   `Learning` does not hold manifests — importing
//!   [`ProviderManifest`](crate::provider::ProviderManifest) here would
//!   couple this module to a type it has no other reason to know about — so
//!   it holds the *answer* instead, as a plain set of ids:
//!   [`Learning::sync_plaintext_providers`] is how whoever does hold the
//!   registry (`hopd`'s daemon wiring, from `ProviderHost::manifests()`)
//!   hands it over. The set is never restored from a loaded file —
//!   [`Learning::load`] always starts it empty, hashing every provider,
//!   until something calls `sync_plaintext_providers` with the real
//!   registry — see that method's own doc comment for why an untrusted file
//!   granting itself plaintext persistence would defeat the whole point of
//!   moving this decision to the manifest. A provider absent from the
//!   synced set — one that never registered, or one whose registration this
//!   process has not learned about yet — hashes by the same default, which
//!   is issue #72's fail-closed requirement.
//! - Every key gets a provider dimension (issue #72). Issue #39 closed the
//!   shape half of Decision 2 and left the other half open on purpose: a
//!   provider that answers honestly at the manifest level can still present
//!   another provider's item id and collect every boost the genuine provider
//!   earned on it, because neither `global_frequency` nor `selections` ever
//!   recorded *which provider* an id came from. [`provider_scoped_key`] closes
//!   that: [`persistence_key`] now takes the provider alongside the raw id and
//!   folds both into one key, and `selections`' inner map is keyed the same
//!   way. The fold is not a plain join — see [`provider_scoped_key`]'s doc
//!   comment for why a bare separator is forgeable and what makes this
//!   composition provably not — and [`rekeyed_global_frequency`] now carries
//!   the load-time migration for it too: a v1 entry in the one plaintext shape
//!   this code can attribute honestly (`app:`) is re-attributed to
//!   [`crate::provider::APPS_PROVIDER_ID`], and every other legacy entry —
//!   including one #39 would have re-hashed — is dropped, since a hash taken
//!   without the provider that earned it can never match a fresh lookup again
//!   regardless.
//!
//! Nothing outside `load` and `save` touches the filesystem.
//!
//! # The store is trust-sensitive input to a launch decision
//!
//! This is not merely state that would be a nuisance to lose.
//! [`Learning::boost_for`] is what `Pipeline::assemble` adds to an item's
//! ranking score, which the ranker computes as `fuzzy + weight + boost`, and
//! a boost is meant to be strong enough to override match quality —
//! `CONTEXT.md` says so in as many words. A single entry in the persisted
//! table is worth up to `FREQ_BOOST_CAP` (60) on its own, more than the
//! entire kind-weight spread (30 for a window down to 6 for a utility
//! kind), so it outweighs every distinction the ranker draws between kinds
//! and is added on top of whatever the query itself matched.
//!
//! The boost only reaches an item some provider actually produced, so the
//! file alone puts no item into the list. What it decides is which of the
//! items the user *does* see comes first — including an item whose id was
//! planted by whoever wrote the file, since a `.desktop` in
//! `~/.local/share/applications` costs the same one write. Position one in
//! a launcher is what the user presses Enter on, which is why whether this
//! file is trusted has to be settled in [`Learning::load`].
//!
//! What the guards here achieve is narrow, and worth stating exactly:
//!
//! - A store on a version this code does not write is refused, in either
//!   direction: an older format and a newer one are equally never read
//!   under this one's semantics. `STORE_VERSION` prices what refusing each
//!   direction costs a user.
//! - A `last_ms` ahead of the load instant is clamped back to it, so decay
//!   runs from that instant rather than never. An unclamped future stamp
//!   switched decay off outright — `apply_decay` returns the raw,
//!   undecayed value while `now <= last_ms` — and no `save` could age it
//!   out of the file either.
//!
//! What they do not achieve is the larger half, and the clamp's half is
//! narrower than it looks. A clamped entry is stamped *now*, so
//! `apply_decay` sees an age of zero and returns the full undecayed boost at
//! that instant; what the clamp removes is the *permanence*, and only once
//! the store is written back, since a store nothing re-saves is re-clamped
//! to a fresh load instant every session. Nor was a future stamp ever an
//! attacker's best move: writing `last_ms` at the current time buys the same
//! boost and trips nothing at all. A store written with a plausible recent
//! `last_ms` and a high `count` is indistinguishable from learning this
//! module recorded itself and gets exactly the boost it asks for; neither
//! guard so much as looks at an entry's id, and nothing here checks the
//! file's owner or mode.
//!
//! That is also why the clamp is not stricter. Penalizing a future stamp —
//! zeroing it, or decaying it hardest — would cost the attacker one edit
//! and cost an honest user with a skewed clock their real learning.
//!
//! Refusing a written store outright means being able to tell that this
//! module wrote it, which is a checksum or a message authentication code —
//! a design decision of its own, deliberately out of scope for issue #38,
//! filed since as issue #88, and implemented nowhere below. The store is
//! better validated than it was. It is not trustworthy, and nothing here
//! should be read as saying so.
//!
//! [`LoadReport`] does not change that, and is easy to misread as though it
//! did. It reports what a load *detected* — the guards above, plus the ones
//! on reaching and parsing the file at all. A store forged to be plausible
//! trips none of them and reports [`LoadReport::Loaded`], so a report never
//! distinguishes a tampered store from an honest one. Distinguishing a
//! tampered store from a merely damaged one is what #88 would buy; what #43
//! bought is that a *damaged* store is no longer indistinguishable from a
//! first run.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use hop_protocol::{ItemId, MAX_ITEM_ID, MAX_PROVIDER_ID, MAX_QUERY_TEXT};

use crate::provider::APPS_PROVIDER_ID;

/// The maximum boost [`Learning::boost_for`] can ever return. Must sit
/// strictly below the alias boost constant (180.0, arriving with the
/// aliases module in M1.6) — aliases are an explicit user instruction and
/// must always beat learned behavior. Exported so the aliases module and
/// its tests can assert that relationship directly.
///
/// This is the ceiling on the *sum* `boost_for` returns, and no single
/// persisted entry reaches it alone: an entry off disk contributes only
/// `frequency_boost`, capped at `FREQ_BOOST_CAP` (60), and the remaining 25
/// can come only from `query_boost`, whose `selections` table is in-memory
/// and never written (see `PersistedLearningStore`). Both figures matter for
/// a different comparison — 85 against the alias boost above, and 60 against
/// the kind weights, which it already exceeds across their whole range. The
/// module docs work through what one entry being worth that much means for a
/// store that was written rather than learned.
pub const LEARNING_BOOST_CAP: f32 = 85.0;

/// The store format this code understands: the `version` [`Learning::save`]
/// writes, and the only one [`Learning::load`] will read. Named once here
/// rather than left as a bare `1` at each site that means it.
///
/// A store on any other version is refused whole — never read entry by
/// entry under this version's semantics — and `load` falls back to an empty
/// state, exactly as it does for a file it cannot parse.
///
/// The resulting *state* is the same as a parse failure's; the resulting
/// *report* is not, and conflating the two would be wrong in the direction
/// that matters. A store on another version is refused for what it announces
/// rather than for being damaged — which is why the version is read on its own
/// before any parse is attempted, so that a later format this code genuinely
/// cannot parse is still reported as a version and not as damage. The version
/// it claimed rides on [`LoadReport::UnrecognizedVersion`], so a caller can
/// tell the two costs below apart: which of them a user is paying is exactly
/// `found` against this constant.
///
/// The refusal is not symmetric in what it costs. An *older* store is
/// learning already lost to whichever change bumped the version. A *newer*
/// one is a live store, written by a later hop, that an older binary now
/// discards: a user who downgrades, or who runs two versions against one
/// state directory, loses their learning outright rather than half of it.
/// That is still the right trade, because the alternative is not "keep the
/// learning" — it is parsing a v2 file under v1 rules for as long as the
/// field names happen to still fit, which misreads the table that decides
/// what the user launches and announces nothing while it does. Lost
/// learning is rebuilt by using hop; a table silently meaning something
/// else is never noticed at all.
const STORE_VERSION: u32 = 1;

// --- Constants (unchanged from the salvage) ---

const MAX_QUERIES: usize = 500;
const MAX_ITEMS_PER_QUERY: usize = 20;
const MAX_GLOBAL_ENTRIES: usize = 1000;

const QUERY_BOOST_PER_COUNT: i32 = 15;
const QUERY_BOOST_CAP: i32 = 150;

const FREQ_BOOST_PER_COUNT: i32 = 3;
const FREQ_BOOST_CAP: i32 = 60;

/// The decimal digit count of [`MAX_PROVIDER_ID`] (64) — two digits.
/// [`provider_scoped_key`]'s length prefix writes a provider's byte length in
/// ordinary decimal, so the longest that prefix can ever be is the digit
/// count of the longest legal provider length, and that has to be spelled out
/// rather than left to `MAX_PROVIDER_ID`'s own magnitude: a `usize` constant
/// carries no notion of "how many digits", so [`MAX_PERSISTED_KEY_LEN`] below
/// would silently under-count if this were computed by eye instead of pinned.
/// `tests::max_provider_id_decimal_digits_matches_its_own_digit_count` holds
/// the two in step, so a future change to `MAX_PROVIDER_ID` that crosses a
/// power of ten fails a test here instead of quietly shrinking a ceiling this
/// module derives from it.
const MAX_PROVIDER_ID_DIGITS: usize = 2;

/// The longest byte length [`provider_scoped_key`] can ever produce, and
/// therefore the longest a [`persistence_key`] output — the only thing
/// [`Learning::record`] ever inserts as a `global_frequency` key — can be:
/// [`MAX_PROVIDER_ID_DIGITS`] digits, a `:`, up to [`MAX_PROVIDER_ID`] bytes
/// of provider, a second `:`, and the longest id-part [`persistence_key`] can
/// produce. That last figure is [`MAX_ITEM_ID`] itself, not the hash branch's
/// fixed 71 bytes (`"sha256:"` plus 64 hex digits): the plaintext branch can
/// write a plaintext-eligible provider's raw id verbatim up to the full
/// item-id bound, which is longer, so the plaintext branch is the one that
/// sets this ceiling.
const MAX_PERSISTED_KEY_LEN: usize = MAX_PROVIDER_ID_DIGITS + 1 + MAX_PROVIDER_ID + 1 + MAX_ITEM_ID;

/// The most bytes [`Learning::load`] will read from a store file. A file
/// over this is refused whole rather than truncated to fit: a prefix of a
/// store is not a smaller store.
///
/// Derived from the two constants that bound a persisted store's shape,
/// rather than picked as a round number:
///
/// ```text
///   MAX_GLOBAL_ENTRIES               1 000  entries that can reach `save`
///                                           (neither row is a bound `save`
///                                           itself applies — see below for
///                                           what enforces each)
///   x  MAX_PERSISTED_KEY_LEN x 6     24 984  one entry's key: a
///                                           provider-scoped key at its
///                                           bound (issue #72 — see that
///                                           constant), every content byte a
///                                           C0 control — which `serde_json`
///                                           writes as `\u001f`, six
///                                           characters for one byte, the
///                                           worst expansion JSON string
///                                           escaping has
///   +  per-entry overhead              128  82 counted, rounded up: the
///                                           quotes, colon, space, braces and
///                                           comma around one entry (7), the
///                                           indentation and newlines
///                                           `to_string_pretty` adds (24),
///                                           and the two numeric fields with
///                                           their names, at full width —
///                                           `count` a 10-digit u32,
///                                           `last_ms` a 20-digit u64 (51)
///                              ------------
///                                25 112 000  bytes, ~23.9 MiB
/// ```
///
/// The bytes per entry that rounding leaves spare also absorb the document's
/// own envelope (`version`, the `global_frequency` key, the enclosing
/// braces), which is under a hundred bytes and does not warrant a term of its
/// own.
///
/// # What actually enforces the two rows
///
/// Neither of them is a bound [`Learning::save`] applies. `save` purges by
/// retention and canonicalizes — both of which can only shrink the map — and
/// then writes whatever `global_frequency` holds, however many entries that
/// is and however long their keys are. Both rows hold transitively, and it
/// is worth writing down through what.
///
/// `MAX_PERSISTED_KEY_LEN` takes two enforcements, not one, the same shape
/// issue #37 gave `MAX_ITEM_ID` before it. [`provider_scoped_key`] bounds
/// every key [`Learning::record_launch`]'s write path can produce — it can
/// only ever be as long as a legal provider and a legal id-part allow — and
/// bounds nothing that arrives off disk: `global_frequency` is a
/// `HashMap<String, _>`, so a parse imposes no length on its keys and calls
/// no key-building function to impose one. `purge_and_bound` is what covers
/// that second half, checking a loaded key's raw length against this
/// constant rather than against `MAX_ITEM_ID` alone, precisely because a
/// provider-scoped key legitimately runs past `MAX_ITEM_ID` now — a bound
/// that still checked `MAX_ITEM_ID` there would drop this module's own
/// freshly recorded, maximally-long entries on their very next load, the
/// exact restart-survival failure issue #72's brief rules out.
///
/// `MAX_GLOBAL_ENTRIES` is enforced by `record`, which has always evicted
/// down to it, and by `purge_and_bound`, also as of issue #37.
///
/// What none of that amounts to is a bound on every `Learning` in existence.
/// `Learning` is public and derives `Deserialize`, and the generated impl is
/// written inside this module, so a private field is no barrier: an outside
/// caller can parse a `Learning` straight from JSON — the very route
/// [`Learning::load`]'s second branch takes, which is why that branch calls
/// `purge_and_bound` rather than inheriting a guarantee from somewhere — and
/// building one in-module, as this module's own tests do, is another way in.
/// A `Learning` obtained either way is bounded by nothing, and `save` would
/// write it out exactly as it found it.
///
/// So this ceiling's guarantee is about the round trip this module owns: a
/// store whose state came from [`Learning::load`] or from `record`, saved,
/// fits under it. That is the case that has to hold, because it is the one
/// hop runs.
///
/// # That the round trip closes
///
/// With both rows enforced on the way in, it does. The largest store a load
/// can hand back is `MAX_GLOBAL_ENTRIES` entries keyed at
/// `MAX_PERSISTED_KEY_LEN` bytes: every other dimension of an entry is
/// bounded by its own type, and what `save` does before writing — retention
/// purging, canonicalization — can only drop an entry, merge two, or shorten
/// a key. That is exactly the store the maximal test builds. A store that
/// survives a load therefore saves to a file the next load accepts.
///
/// Without the key bound it did not close, and the entry cap could not have
/// closed it: a hand-written store in *compact* JSON can sit under this
/// ceiling with keys far past `MAX_PERSISTED_KEY_LEN`, and `save`
/// re-serializes it with `to_string_pretty`'s indentation and spacing on
/// top — over the ceiling, and unreadable by the very next load. The entry
/// count in that story is legal throughout; only the key length is not.
///
/// The ceiling **must** comfortably admit any store this module writes from
/// state it produced, or a legitimate full store would fail to reload and
/// the guard meant to protect a user's learning would be what discarded it.
/// That requirement is held as a test rather than as a claim here:
/// `the_largest_store_save_can_write_still_reloads_intact` builds a store
/// sitting on every one of those maxima at once, saves it, measures the file
/// against this ceiling and reloads it. What makes those the real maxima is
/// the transitive enforcement above, whose load-path half is pinned
/// separately by `a_store_over_the_entry_cap_is_evicted_down_to_it_on_load`
/// and `a_persisted_key_over_the_bound_is_dropped_on_load`.
///
/// This bounds bytes and nothing else. It is no bound on how many entries a
/// store holds — a file a tenth of this size can still carry tens of
/// thousands of tiny entries — which is `MAX_GLOBAL_ENTRIES`'s job,
/// applied separately after the parse.
const MAX_STORE_BYTES: u64 = (MAX_GLOBAL_ENTRIES * (MAX_PERSISTED_KEY_LEN * 6 + 128)) as u64;

/// 30 days in milliseconds — half-life for decay.
const DECAY_HALF_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// 90 days in milliseconds — quarter-life for decay.
const DECAY_QUARTER_MS: u64 = 90 * 24 * 60 * 60 * 1000;
/// Hard retention cutoff for persisted learning data.
const PERSIST_RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;

// --- Data types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LearningEntry {
    /// Saturated at deserialization — see [`saturating_count_i32`] for why.
    #[serde(deserialize_with = "deserialize_saturating_count")]
    count: u32,
    last_ms: u64,
}

/// Frecency state: per-query selection history plus a global launch
/// frequency table, both decayed by recency and capped in size.
///
/// State is private. The contract this slice was specified against is
/// [`Learning::load`], [`Learning::save`], [`Learning::record_launch`] and
/// [`Learning::boost_for`]; [`Learning::reset`], [`Learning::recent_launches`],
/// [`Learning::frequent_launches`] and [`Learning::is_empty`] are also
/// public, carried over from the salvage as-is for later milestones (surfacing
/// learning insights to the user is explicitly out of scope here).
///
/// `Clone` is what lets a caller take a snapshot of the in-memory state and
/// let go of whatever lock protects it before doing something slow with the
/// copy — `hopd`'s `HostSource::record_launch` is exactly that caller: it
/// clones the pipeline's `Learning` while still holding the pipeline's lock
/// (a fast, in-memory copy — every dimension a save touches is bounded, see
/// `MAX_STORE_BYTES` above), then saves the clone after releasing it, so a
/// blocking `save` never holds a lock anything else is waiting on. `Clone`
/// duplicates data an outside caller could already reach no other way than
/// through what `Learning` itself already exposes; it grants no new route to
/// bypass the private fields' invariants the way `Deserialize` does (see
/// below), since a clone can only ever hold a copy of an already-valid
/// `Learning`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Learning {
    /// Only ever read during a [`Learning::load`], where the second parse
    /// branch needs somewhere to put a file's `version` before checking it
    /// against `STORE_VERSION`. Its value means nothing once `load` has
    /// returned, and the two constructors disagree about it harmlessly:
    /// `Default` zeroes it, `Learning::empty` sets `STORE_VERSION`.
    /// [`Learning::save`] consults neither — it writes `STORE_VERSION`, for
    /// the reason given there.
    version: u32,
    #[serde(default, skip_serializing)]
    selections: HashMap<String, HashMap<String, LearningEntry>>,
    global_frequency: HashMap<String, LearningEntry>,
    /// The provider ids [`persistence_key`] currently treats as safe to
    /// persist in the clear — see [`Learning::sync_plaintext_providers`] for
    /// how this is set and why it is set from outside rather than computed
    /// here.
    ///
    /// `#[serde(skip)]`, not merely `skip_serializing` the way `selections`
    /// is: `selections` is excluded from the *written* shape because raw
    /// query text has no business on disk at all, but a `Learning` is still
    /// free to *read* one back from a file that happens to carry one (a
    /// hand-edited store, say) with no consequence, because nothing downstream
    /// trusts it either way. This field is different — reading one back from
    /// an untrusted file would let a forged store grant itself plaintext
    /// persistence for every lookup this process makes from then on, which is
    /// exactly the authority issue #72 moves to the manifest and away from
    /// anything the id (or, here, the file) can assert about itself. `skip`
    /// excludes it in both directions: every deserialized `Learning` —
    /// through [`Learning::load`]'s own parse or through any other route that
    /// reaches `Learning`'s `Deserialize` impl (see `MAX_STORE_BYTES`'s doc
    /// comment on why that impl is reachable from outside this module at
    /// all) — starts this set at `HashSet::default()`, empty, hashing every
    /// provider until [`Learning::sync_plaintext_providers`] is called with
    /// the real registry.
    #[serde(skip)]
    plaintext_providers: HashSet<String>,
}

/// The on-disk shape: per-query selections are intentionally left out, so
/// raw query text never lands on disk.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedLearningStore {
    version: u32,
    global_frequency: HashMap<String, LearningEntry>,
}

/// The one thing read out of a store document before anything else: the
/// `version` it announces. Every other field is ignored, so this parses
/// whatever a later hop's format turns out to be, as long as it is a JSON
/// object that still says which format it is.
///
/// Reading the version on its own is what makes the check a check. Both full
/// parses below must deserialize a document *completely* before its `version`
/// field is reachable, and a version is bumped precisely because the shape
/// changed — so a store written by a later hop generally fails both parses,
/// and checking the version afterwards would only ever catch the one v2 that
/// happened to keep every v1 field. That is the reverse of what
/// [`LoadReport::UnrecognizedVersion`] promises: the realistic later store
/// would report [`LoadReport::Malformed`] and tell a user who downgraded that
/// their live learning was damaged.
///
/// This also subsumes what issue #38 achieved by checking the version inside
/// both parse branches. That guarded against the two shapes drifting apart;
/// this does not depend on either shape at all, so the per-branch checks are
/// gone rather than kept alongside it — two sites deciding one outcome is how
/// they come to disagree.
///
/// What it deliberately does not do is treat every unparseable document as a
/// version problem. A document with no `version` to read announces nothing and
/// is not a store, so it stays [`LoadReport::Malformed`].
#[derive(Deserialize)]
struct StoreVersionProbe {
    version: u32,
}

/// What a load noticed about the store it read: that it loaded, or which one
/// of the fallbacks it took instead. [`Learning::load_reporting`] returns one
/// alongside the store; [`Learning::load`] discards it.
///
/// One distinguishable outcome per condition that ends in an empty state, and
/// no outcome shared by two of them — counting a variant's payload as part of
/// the outcome, since [`LoadReport::Unreadable`] is one outcome per
/// [`std::io::ErrorKind`] rather than one for every way a read can fail. That
/// is the whole point (issue #43): before this existed, an absent file, an
/// unreadable one, a damaged one and one written by a later hop all produced
/// the identical value, so learning wiped by a corrupted disk was reported
/// nowhere, and a state directory that had become unreadable was
/// indistinguishable from a first run — every session, for ever.
///
/// # Why a report beside the store, rather than a `Result`
///
/// `load` returning `Result<Learning, LoadReport>` was the obvious shape and
/// is the one rejected. A `Result` says the value on the error side does not
/// exist, and here it always does: every condition below yields a usable
/// empty store, which is the degradation the issue explicitly keeps. Modelling
/// that as an error would put an `unwrap_or_else(|_| ...)` at every call site
/// — reconstructing, per caller, the empty store this function already built —
/// which is exactly the per-caller burden the brief rules out. A pair says
/// what is true instead: there is always a store, and there is always
/// something to say about where it came from.
///
/// Two variations were rejected for narrower reasons. Making `load` itself
/// return the pair would force every caller to acknowledge a report it may not
/// want, and `load`'s signature is fixed. Hanging the report off [`Learning`]
/// as a field would make a property of one load event look like a property of
/// the store, and would have to mean something for a `Learning` that never
/// came from a file at all — `Learning::default()`, `record`'s output — where
/// it means nothing.
///
/// This crate does not answer the question the same way everywhere, and the
/// difference is the point rather than an inconsistency.
/// `Aliases::from_json` does return a `Result` and does refuse a config it
/// cannot read, because an alias is an explicit user instruction and one that
/// quietly stopped working is a bug the user cannot act on — that module's
/// docs argue it at length. Learning is inferred rather than instructed: it is
/// rebuilt by using hop, and a store that cannot be read must not stop hop from
/// starting. So degrading is right here where refusing is right there, and the
/// report is what recovers the one thing refusing would have given for free.
///
/// # Not `#[non_exhaustive]`, deliberately
///
/// This matches what the crates already do: every public enum in `hop-core`
/// and `hop-protocol` is exhaustive but `AliasError`, which opts in and says
/// why. Following the default needs no argument of its own; departing from it
/// would.
///
/// The reason not to depart is that `hop-core` is consumed only from inside
/// this workspace, so the thing `#[non_exhaustive]` buys — adding a variant
/// without breaking a downstream crate — is worth nothing here, while
/// exhaustiveness does buy something: `cargo check` names every site that has
/// to think again when a fallback path is added.
///
/// What that is *not* is a guarantee that no caller ever writes a `_` arm.
/// [`std::io::ErrorKind`] is itself `#[non_exhaustive]`, so a caller that
/// looks inside [`LoadReport::Unreadable`] writes one regardless of anything
/// decided here.
///
/// # What a report is evidence of, and what it is not
///
/// It says what this load *detected*. It is not a verdict on the store.
/// [`LoadReport::Loaded`] means the bytes passed every guard `load` applies —
/// the byte ceiling, the version check, the parse, the key bound, the
/// timestamp clamp — and nothing more. A store written by someone else with a
/// plausible recent `last_ms` and a high `count` passes all of those and
/// reports `Loaded`, because nothing here can tell it from learning this
/// module recorded itself; the module docs work through what that buys an
/// attacker. Telling a *tampered* store from a merely damaged one means being
/// able to tell that this module wrote the bytes, which is a checksum or a
/// message authentication code and is issue #88. Until that exists, no variant
/// below reports tampering and none should be read as ruling it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadReport {
    /// A store was read, parsed, and accepted. The state returned beside this
    /// came off disk.
    Loaded,
    /// Nothing at the path — the ordinary first run. This is the report for an
    /// I/O error of kind `NotFound` and nothing else, which is what separates
    /// it from [`LoadReport::Unreadable`].
    ///
    /// `NotFound` is the OS's answer, not a claim about intent: a dangling
    /// symlink and a state directory whose leading components do not exist
    /// both arrive here too, and nothing on the load path tells those from a
    /// user who has simply never launched anything.
    Absent,
    /// The path resolves to something that is not a regular file — a
    /// directory, a FIFO, a device. Reported separately because such a path is
    /// neither absent nor damaged: it exists, and it is not a store.
    NotARegularFile,
    /// The store could not be read, and the reason was not that it is absent.
    /// Carries the [`std::io::ErrorKind`] that said so: `PermissionDenied` for
    /// a file this process may not open or a directory it may not search,
    /// `InvalidData` for bytes that are not UTF-8, and whatever else the
    /// underlying I/O reported.
    ///
    /// Stated that way round on purpose. A permission denial on a leading
    /// directory arrives here without anything having established that a store
    /// is there at all, so "the file exists and cannot be read" would be a
    /// claim this variant is in no position to make.
    ///
    /// `InvalidData` means the store's own bytes are not UTF-8, and not that
    /// the bounded read cut one in half: the byte ceiling is applied before
    /// anything is decoded, so an over-size file is [`LoadReport::TooLarge`]
    /// before a decode is attempted. `read_bounded_store` has the ordering.
    ///
    /// The kind is carried rather than flattened because those are different
    /// problems with different fixes, and the outcome this variant most needs
    /// to stay distinct from is [`LoadReport::Absent`]: they are one
    /// `io::ErrorKind` apart in the implementation and nothing alike to a user.
    Unreadable(io::ErrorKind),
    /// The file holds more than `MAX_STORE_BYTES`. Distinct from
    /// [`LoadReport::Malformed`]: the bytes may be a flawless store, and are
    /// refused for their size alone, which `MAX_STORE_BYTES` explains and
    /// prices.
    ///
    /// "Size alone" is load-bearing and was once not true. The ceiling is
    /// applied to the bytes before they are decoded, so an over-size store is
    /// reported here whatever the bounded read's cut happened to land on;
    /// `read_bounded_store` says what went wrong when the decode came first.
    TooLarge,
    /// The bytes are not a store document: truncated JSON, something that is
    /// not JSON at all, valid JSON with no `version` field to read, or a
    /// document on `STORE_VERSION` whose body this code cannot parse.
    ///
    /// A document that names a version this code does not write is *not* here
    /// — it is [`LoadReport::UnrecognizedVersion`], however unparseable the
    /// rest of it is. What is left for this variant is a document that never
    /// said which format it was, or said this one and then was not it.
    ///
    /// Damage is the reachable cause, and this variant does not identify one.
    /// It names what the parse could tell — that these bytes are not a store —
    /// which is the same whether a disk corrupted them or somebody wrote them
    /// deliberately.
    Malformed,
    /// The document announces a `version` that is not `STORE_VERSION`, so the
    /// store is refused whole. `found` is the version it claimed.
    ///
    /// Emphatically not [`LoadReport::Malformed`], and for a document of any
    /// shape: the version is read on its own before either full parse (see
    /// `StoreVersionProbe`), so a store from a later hop is refused for what
    /// it says about itself whether or not it still resembles this version's
    /// layout — which it usually will not, a version being bumped precisely
    /// because the layout changed. All this variant asserts is that the
    /// document said which format it was and it was not this one.
    ///
    /// `found` is carried because the two directions cost a user quite
    /// different things — `STORE_VERSION` prices each — and comparing it
    /// against `STORE_VERSION` is the only way to tell an abandoned older
    /// store from a live newer one this binary is too old to read.
    UnrecognizedVersion { found: u32 },
}

impl LoadReport {
    /// The report for an I/O error raised on the load path.
    ///
    /// `NotFound` is the single kind that means the store is *absent*;
    /// everything else means it is there and this process could not read it.
    /// That one line is what stops a permission denial from being reported as
    /// a first run, and it is the whole of the distinction — there is no other
    /// signal available at the point either error is raised.
    fn from_io(err: &io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::NotFound => LoadReport::Absent,
            kind => LoadReport::Unreadable(kind),
        }
    }
}

// --- Helper functions (unchanged from the salvage) ---

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Apply recency decay to a raw boost value.
/// Returns full value if within half-life, halved if within quarter-life, quartered beyond.
///
/// # Why `now <= last_ms` still returns the raw value
///
/// A stamp off disk can no longer arrive here ahead of the clock:
/// `Learning::purge_and_bound` clamps a `last_ms` that sits ahead of the
/// load instant back to it — and leaves every other stamp alone — which is
/// what stops a future-dated entry from holding an undecayed boost for
/// ever. The branch is kept regardless, deliberately,
/// for the two reasons below — and the paragraph after them says why it is
/// not replaced by something harsher instead.
///
/// It is this function's guard against `now - last_ms` underflowing, and
/// that case is reachable without anything forged: `record` stamps entries
/// with `now_ms()`, and a wall clock that then moves backwards — an NTP
/// correction, a laptop waking with a stale RTC — leaves an in-memory entry
/// stamped after the next reading. Without the branch that subtraction
/// panics in debug and wraps to an enormous age in release.
///
/// It also costs nothing against the alternative that keeps the function
/// total: `now.saturating_sub(last_ms)` yields an age of 0, 0 is inside the
/// half-life, and the half-life arm returns `raw` — the same value by a
/// longer route. The branch only says so where it can be read.
///
/// And the remaining alternative — penalizing the entry, returning 0 or
/// decaying it hardest — is rejected because the reachable case is the
/// honest one. It would spend a user's real learning on a clock correction
/// they did not make, and it would not make a written store any less
/// believed, which is the thing that actually wants fixing and which
/// nothing in this module can do (see the module docs).
///
/// What the branch costs is unchanged, but no longer unbounded: an entry
/// stamped ahead of the clock is boosted as though it had just been
/// launched, until the clock passes it. Off disk that is now at most the
/// instant of the load; in memory it is however far the clock jumped back.
fn apply_decay(raw: i32, last_ms: u64, now: u64) -> i32 {
    if now <= last_ms {
        return raw;
    }
    let age = now - last_ms;
    if age <= DECAY_HALF_MS {
        raw
    } else if age <= DECAY_QUARTER_MS {
        raw / 2
    } else {
        raw / 4
    }
}

/// Saturating conversion from a persisted or in-memory `count` to the `i32`
/// scale [`Learning::query_boost`] and [`Learning::frequency_boost`] do
/// their arithmetic in.
///
/// A plain `as i32` cast wraps negative once `count` exceeds `i32::MAX` —
/// `4_000_000_000_u32 as i32` is roughly `-294_967_296` — and every step
/// downstream (`saturating_mul`, [`apply_decay`], the final cap) treats that
/// as a genuine negative amount rather than an error, so one corrupted or
/// overgrown count could suppress a boost outright instead of merely
/// capping it. Saturating instead of rejecting matches [`Learning::load`]'s
/// own policy of degrading bad data rather than discarding it (see its doc
/// comment): an out-of-range count should cap out, not flip sign.
///
/// This is the one place that ceiling is defined. `deserialize_saturating_count`
/// applies it at the point a [`LearningEntry`] is deserialized,
/// [`Learning::query_boost`] / [`Learning::frequency_boost`] apply it again
/// themselves, and [`rekeyed_global_frequency`] applies it once more when a
/// load merges two colliding entries (see its own doc comment for why) — so
/// a count is safe whether it just came off disk, was built directly in
/// memory (a test, say), grew past the line via `record`'s `saturating_add`,
/// or was just merged from two counts that were each already at the line.
fn saturating_count_i32(count: u32) -> i32 {
    count.min(i32::MAX as u32) as i32
}

/// Bounds `count` to what [`saturating_count_i32`] can convert without
/// wrapping, at the point a [`LearningEntry`] is deserialized — see its doc
/// comment for why saturating rather than rejecting is correct here.
fn deserialize_saturating_count<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u32::deserialize(deserializer)?;
    Ok(saturating_count_i32(raw) as u32)
}

/// Evict least-recently-used entries from an inner map (result_id -> LearningEntry)
/// until the map size is at most `max`.
fn evict_lru_map(map: &mut HashMap<String, LearningEntry>, max: usize) {
    while map.len() > max {
        if let Some(oldest_key) = map
            .iter()
            .min_by_key(|(_, entry)| entry.last_ms)
            .map(|(key, _)| key.clone())
        {
            map.remove(&oldest_key);
        } else {
            break;
        }
    }
}

/// Evict least-recently-used outer keys from the selections map
/// until the map size is at most `max`. The "age" of an outer key is
/// the maximum last_ms across its inner entries.
fn evict_lru_outer(map: &mut HashMap<String, HashMap<String, LearningEntry>>, max: usize) {
    while map.len() > max {
        if let Some(oldest_key) = map
            .iter()
            .map(|(key, inner)| {
                let max_ms = inner.values().map(|e| e.last_ms).max().unwrap_or(0);
                (key.clone(), max_ms)
            })
            .min_by_key(|(_, ms)| *ms)
            .map(|(key, _)| key)
        {
            map.remove(&oldest_key);
        } else {
            break;
        }
    }
}

/// Folds `provider` into `id_part` so that no `(provider, id_part)` pair can
/// ever produce the same string as a *different* pair — the composition
/// [`persistence_key`] and `selections`'s per-provider lookup both build
/// their final key from, and the fix for issue #72: before this existed,
/// both maps were keyed on the bare id alone, so a provider that presented
/// another provider's item id collected every boost the genuine provider had
/// earned on it.
///
/// # Why a plain separator does not work
///
/// `provider` is bounded at [`MAX_PROVIDER_ID`] (64 bytes) but otherwise
/// unconstrained — no character-set rule, nothing that keeps it from
/// containing whatever separator this function might pick — and `id_part` is
/// built from an id bounded at [`MAX_ITEM_ID`] (4 096 bytes) with the same
/// lack of a content rule. `format!("{provider}:{id_part}")` is forgeable for
/// exactly that reason: a provider `"apps:app"` presenting id `"firefox"`
/// produces `"apps:app:firefox"`, the identical string the honest provider
/// `"apps"` produces for id `"app:firefox"`. Nothing about the choice of
/// separator matters here — the *provider itself* chooses both halves of the
/// string on either side of the boundary, and a bare join can never tell
/// which side a separator that appears inside the provider's own id belongs
/// to. Any single fixed character loses the same way; the ambiguity is
/// structural, not a wrong choice of glyph.
///
/// # The composition, and why it cannot be forged
///
/// `format!("{}:{}:{}", provider.len(), provider, id_part)` — the provider's
/// own byte length, written in ordinary decimal with no leading zero (the
/// way [`usize`]'s `Display` always writes it), then a `:`, then exactly that
/// many bytes of `provider`, then a second `:`, then `id_part` verbatim. This
/// is a length-prefixed (netstring-style) encoding, and its defining property
/// is that the prefix says exactly where the provider ends — nothing an
/// attacker puts inside `provider` or `id_part` can move that boundary,
/// because the boundary is read off a number that was fixed before either
/// string's bytes are ever consulted.
///
/// Concretely: suppose two calls, `(provider_a, id_a)` and `(provider_b,
/// id_b)`, produce the same output string `s`. Read `s`'s leading digits up
/// to its first `:` — call that digit run `d`. Both calls wrote a canonical
/// decimal `d` (no leading zero) immediately followed by `:`, and a canonical
/// decimal representation of a given value is unique, so `d` names one value
/// both calls agree the provider length is; call it `n`. If `provider_a` and
/// `provider_b` have different lengths, their canonical decimal prefixes
/// differ as *strings* — one is never a textual prefix of the other once
/// each is immediately followed by `:`, since `:` is not a decimal digit and
/// so cannot be mistaken for one more digit of a shorter number — so `s`
/// cannot agree with both, a contradiction. Hence `provider_a.len() ==
/// provider_b.len() == n`. `s`'s next `n` bytes after the first `:` are
/// therefore `provider_a`'s bytes under one reading and `provider_b`'s under
/// the other, so those `n` bytes are identical: `provider_a == provider_b`.
/// What remains of `s` after that shared prefix (`n`'s digits, `:`,
/// `provider_a`, `:`) is `id_a` under one reading and `id_b` under the
/// other, so `id_a == id_b` too. Two calls that produce the same string were
/// therefore made with the same arguments — the composition is injective, so
/// a hostile provider changing either half of its own call can only ever
/// move its own output, never land on another provider's.
///
/// # What this does not do
///
/// It says nothing about whether `id_part` itself is safe to write in the
/// clear — that is [`persistence_key`]'s decision, made before this function
/// ever runs, and this function composes the *result* of that decision with
/// `provider`, not the raw id. And it composes exactly what it is given: two
/// different providers each answering with the *same* honest id still
/// produce two different keys, as the proof above requires — `evil`
/// presenting `app:firefox` and `apps` presenting `app:firefox` must never
/// collide, and now provably do not.
fn provider_scoped_key(provider: &str, id_part: &str) -> String {
    format!("{}:{}:{}", provider.len(), provider, id_part)
}

/// Whether `key` is already shaped like [`provider_scoped_key`]'s own
/// output — `<n>:<provider>:<id-part>`, where `n` is the exact decimal byte
/// length of `provider`, written the way [`usize`]'s `Display` writes it (no
/// leading zero), immediately followed by that many bytes and then a `:`.
/// See [`provider_scoped_key`]'s doc comment for why that shape can only be
/// produced by [`provider_scoped_key`] itself, over the provider and
/// id-part it was actually called with.
///
/// [`rekeyed_legacy_key`] is the one caller of the boolean form
/// ([`is_already_provider_scoped`]): a key already in this shape is this
/// module's own prior output round-tripping through a save and a load, and
/// is left untouched rather than run through the legacy migration below it,
/// which is only for a v1 store predating issue #72's provider dimension.
/// No key issue #39-era code ever wrote can accidentally match this shape:
/// every one of the four shapes it produced (`app:`, `utility:`,
/// `web-search:`, `sha256:`) opens with an ASCII letter, never a digit, so a
/// legacy key can never be mistaken for this module's current output.
///
/// Returns the parsed `(provider, id_part)` on success — used directly by
/// [`Learning::rehash_entries_for_providers_no_longer_opted_in`], which
/// needs both halves of an already-scoped key, not just the yes/no
/// [`is_already_provider_scoped`] answers.
fn parse_provider_scoped_key(key: &str) -> Option<(&str, &str)> {
    let (len_digits, rest) = key.split_once(':')?;
    let provider_len: usize = len_digits.parse().ok()?;
    // Reject a non-canonical length field (a leading zero, or anything else
    // `parse::<usize>` tolerates that `Display` never writes) so the digits
    // consumed here are exactly the digits `provider_scoped_key` itself
    // would have written — see that function's doc comment for why the
    // prefix has to be canonical for the shape to be unambiguous at all.
    if len_digits != provider_len.to_string() {
        return None;
    }
    if rest.as_bytes().get(provider_len) != Some(&b':') {
        return None;
    }
    Some((&rest[..provider_len], &rest[provider_len + 1..]))
}

/// Whether `key` is already shaped like [`provider_scoped_key`]'s own
/// output — see [`parse_provider_scoped_key`], which does the actual
/// parsing this just discards the result of.
fn is_already_provider_scoped(key: &str) -> bool {
    parse_provider_scoped_key(key).is_some()
}

/// Whether `id_part` is exactly the shape [`persistence_key`]'s hash branch
/// produces: `sha256:` followed by 64 lowercase hex characters. Used by
/// [`Learning::rehash_entries_for_providers_no_longer_opted_in`] to avoid
/// hashing an id-part that is already a hash — see that method's doc
/// comment for the one case this still cannot tell apart from a genuine
/// hash (a plaintext id that happens to look like one), which is not new to
/// this function: [`persistence_key`]'s own doc comment already discusses
/// it for the record path.
fn looks_like_a_persistence_hash(id_part: &str) -> bool {
    id_part.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The key [`Learning::record`] and every `global_frequency` lookup use —
/// Decision 2's rule for what a *raw* id, from a *named* provider, looks
/// like once it can reach disk, folded together with issue #72's provider
/// dimension. Every site on the live record/lookup path calls it, so the two
/// are kept in sync by construction rather than by convention.
/// [`Learning::load`]'s re-keying pass is the one caller that does *not*
/// call this directly on every stored key — see [`rekeyed_legacy_key`] for
/// why.
///
/// # The rule
///
/// 1. If `persist_plaintext` is `true`, the id-part is `raw_id` verbatim.
///    Otherwise it is `sha256:` followed by the lowercase hex SHA-256 digest
///    of `raw_id` **as it arrived** — including when `raw_id` already begins
///    `sha256:`, which is hashed again rather than written through; see
///    below.
/// 2. Fold `provider` into that id-part with [`provider_scoped_key`], whose
///    own doc comment has the forgery proof: this step is what stops the
///    scenario issue #72 exists to close — a provider `evil` presenting
///    `app:firefox` must never compute the same key as the genuine `apps`
///    presenting `app:firefox`, and after this step it structurally cannot.
///
/// `persist_plaintext` is not decided here. Issue #39 decided it by
/// inspecting `raw_id`'s own shape (`app:`, or `utility:`/`web-search:` with
/// a payload actually stripped); issue #72 replaced that guess with the
/// manifest's own claim
/// ([`ProviderManifest::ids_are_safe_to_persist_in_the_clear`](crate::provider::ProviderManifest::ids_are_safe_to_persist_in_the_clear)),
/// read off `provider` by whoever calls this function —
/// [`Learning::record`] and [`Learning::frequency_boost`], via
/// `self.plaintext_providers` (see [`Learning::sync_plaintext_providers`]).
/// This function itself no longer looks at `raw_id`'s shape at all: a
/// `calc:`-shaped id from a provider whose manifest opts in persists
/// verbatim, and an `app:`-shaped id from a provider that does not persists
/// hashed — the partition is entirely the caller's to make, and this
/// function only executes it.
///
/// An id shaped exactly like this function's own hash output — `sha256:`
/// plus 64 lowercase hex characters — is *not* treated as already-hashed on
/// the hash branch; it is hashed again, landing under `sha256:` followed by
/// the digest of that whole string.
/// `an_id_beginning_sha256_is_hashed_rather_than_written_through` pins this
/// with the exposing shape the brief asks for: a raw id that is a
/// syntactically valid 64-character hex digest, not merely a short
/// look-alike.
///
/// # Why the plaintext/hash partition is still provable with a provider folded in
///
/// Step 1's id-part is either `raw_id` verbatim (the plaintext branch) or
/// `sha256:` followed by a 64-character hex digest (the hash branch), and no
/// input reaches both — they are `if`/`else` on `persist_plaintext`, a plain
/// `bool`. Step 2 then wraps whichever id-part step 1 produced with
/// [`provider_scoped_key`], which — per its own proof — is injective in
/// `(provider, id_part)`: two calls collide only if both their provider and
/// their id-part were equal. So a plaintext-id-part key and a hashed-id-part
/// key can never collide with each other regardless of provider, and two
/// calls with the *same* id-part collide only when their providers also
/// agree — which is exactly [`Learning::record`] merging repeat launches of
/// the same id by the same provider, not a cross-provider collision.
///
/// A plaintext id-part could, in principle, happen to already start with
/// `sha256:` followed by 64 hex characters — an opted-in provider is free to
/// mint an id shaped exactly like this module's hash output, since nothing
/// about opting in constrains an id's *content*. That does not reopen the
/// partition: what distinguishes the two branches is which key `persistence_key`
/// itself was called with `persist_plaintext` true or false for, not
/// anything inferred from the resulting string's shape after the fact, so a
/// plaintext id that merely *looks* hashed is still found by a lookup that
/// (correctly, for that provider) passes `persist_plaintext: true` again.
///
/// # What the hash is not
///
/// It is not confidentiality against someone who can read the store file:
/// SHA-256 has no secret input here, so anyone who suspects a particular
/// query recovers it by hashing their guess and comparing. What it defends
/// is disclosure to someone who is *not* looking for anything in
/// particular — a backup, a support bundle, a `cat` of a config directory —
/// where a plaintext `calc:$(rm -rf ~)` or a search term is legible on
/// sight and a hex digest is not. Confidentiality against a targeted reader
/// of the file needs a secret (a per-installation salt, at minimum) and is
/// explicitly not this function's job. Nor is `provider` itself hashed or
/// hidden by this function at all — it is folded in for partitioning, not
/// for secrecy, and appears in the clear in every persisted key regardless
/// of which branch step 1 took.
fn persistence_key(provider: &str, raw_id: &str, persist_plaintext: bool) -> String {
    let id_part = if persist_plaintext {
        raw_id.to_string()
    } else {
        format!("sha256:{:x}", Sha256::digest(raw_id.as_bytes()))
    };
    provider_scoped_key(provider, &id_part)
}

/// Re-key every entry of a freshly parsed store's `global_frequency`,
/// merging any two source entries that land on the same key —
/// [`Learning::load`]'s half of the persistence-key rule (see
/// [`persistence_key`]).
///
/// A store predating issue #72's provider dimension — v1, written by #39-era
/// code or earlier — can hold a key in any of the four shapes that code ever
/// wrote: `app:`, `utility:`, `web-search:`, or `sha256:`. None of the four
/// carries a provider, so none can be re-keyed into the injective
/// `(provider, id_part)` space [`persistence_key`] now computes without
/// *inventing* a provider for it — and per option A (the maintainer decision
/// recorded in the issue), only one shape has an invented provider worth
/// trusting: `app:`, whose ids are desktop-entry names, enumerable and not
/// user-authored, so attributing every one of them to
/// [`APPS_PROVIDER_ID`] is attributing them to the provider that in fact
/// minted every `app:` id this code has ever produced — no other provider
/// exists yet that could have. The other three shapes have no such single
/// honest owner: a legacy `utility:`/`web-search:` entry predates any real
/// provider in that namespace, and a legacy `sha256:` entry has already
/// discarded the raw id its hash was computed over, so there is nothing left
/// to re-derive a provider from even in principle. All three, and anything
/// else a v1 file might contain, are simply dropped — dead weight either
/// way, since a hash taken without the provider that earned it can never
/// match what a live [`persistence_key`] lookup computes now, regardless of
/// what this pass did with it.
///
/// [`rekeyed_legacy_key`] is the per-entry decision; this function is the
/// loop plus the merge two entries need when they land on the same key,
/// unchanged in shape from before this issue: counts sum (saturating, the
/// same posture [`deserialize_saturating_count`] takes for the same reason —
/// see [`saturating_count_i32`]'s doc comment), and `last_ms` takes the later
/// of the two, since that is the more recent launch either source entry
/// attests to.
///
/// Unlike before this issue, two *distinct* legacy ids can no longer collide
/// onto one output key on their own: [`rekeyed_legacy_key`]'s app: branch is
/// `provider_scoped_key(APPS_PROVIDER_ID, id)`, injective in `id` for a fixed
/// provider, and its identity branch is trivially injective. The collision
/// this merge still has to handle is cross-branch instead — a v1 store that
/// holds both a plain `app:<rest>` key and a key already shaped like this
/// module's own provider-scoped output for the *same* id, which re-key onto
/// the identical final string — and dropping either would discard real
/// learning the store actually recorded.
/// `rekeying_global_frequency_saturates_a_merged_count_at_i32_max` pins the
/// merge at that reachable collision, not at a same-branch one that can no
/// longer occur.
///
/// Was `canonicalized_global_frequency` and ran inside [`Learning::save`],
/// keying only through what was then `canonicalize_result_id` (removed as
/// of issue #72; see this module's docs) rather than the full
/// [`persistence_key`] rule. Moved to the load path because the key a
/// lookup computes has to be the key a launch was recorded under, across a
/// restart — hashing only on the way to disk would leave `global_frequency`
/// keyed by raw id in memory while a reload keyed it by hash, silently
/// breaking every hashed provider's learning. See [`Learning::record`] and
/// [`Learning::purge_and_bound`] for the two ends of where that moved to.
fn rekeyed_global_frequency(
    input: &HashMap<String, LearningEntry>,
) -> HashMap<String, LearningEntry> {
    let mut out: HashMap<String, LearningEntry> = HashMap::new();
    for (id, entry) in input {
        let Some(key) = rekeyed_legacy_key(id) else {
            continue;
        };
        merge_learning_entry(&mut out, key, entry);
    }
    out
}

/// Merges `entry` into whatever `map` already holds under `key` — summing
/// counts (saturating, the same posture [`deserialize_saturating_count`]
/// takes for the same reason; see [`saturating_count_i32`]'s doc comment)
/// and taking the later of the two `last_ms` values, since that is the more
/// recent launch either source entry attests to — or inserts `entry` fresh
/// if `key` is not yet present.
///
/// The one merge rule this module has, shared by [`rekeyed_global_frequency`]'s
/// load-time migration and
/// [`Learning::rehash_entries_for_providers_no_longer_opted_in`]'s
/// revocation re-hash, so the two callers cannot disagree about what
/// "merge" means — a single collision two entries can land on either way,
/// with one policy rather than two that could drift.
fn merge_learning_entry(
    map: &mut HashMap<String, LearningEntry>,
    key: String,
    entry: &LearningEntry,
) {
    let aggregate = map.entry(key).or_insert(LearningEntry {
        count: 0,
        last_ms: 0,
    });
    aggregate.count = (saturating_count_i32(aggregate.count)
        .saturating_add(saturating_count_i32(entry.count))) as u32;
    aggregate.last_ms = aggregate.last_ms.max(entry.last_ms);
}

/// The key a single stored entry re-keys to, or `None` if the entry does not
/// survive [`rekeyed_global_frequency`]'s load-time migration at all — split
/// out so the per-entry rule reads as one function rather than a branch
/// buried inside the loop and its merge.
///
/// 1. Already shaped like [`provider_scoped_key`]'s own output
///    ([`is_already_provider_scoped`]) — this module's own prior output,
///    round-tripping through a save and a load. Left exactly as it is.
/// 2. A legacy `app:`-prefixed key — re-attributed to
///    [`APPS_PROVIDER_ID`] via [`provider_scoped_key`], carrying the whole
///    `app:<rest>` string through as the id-part unchanged, exactly what
///    [`persistence_key`] would have produced for the same raw id presented
///    by the apps provider.
/// 3. Anything else — dropped. See [`rekeyed_global_frequency`]'s doc
///    comment for why every other legacy shape has no honest provider to
///    attribute it to and is dead weight regardless.
fn rekeyed_legacy_key(id: &str) -> Option<String> {
    if is_already_provider_scoped(id) {
        return Some(id.to_string());
    }
    if id.starts_with("app:") {
        return Some(provider_scoped_key(APPS_PROVIDER_ID, id));
    }
    None
}

/// The `selections` key for `query`, or `None` if that key would be over
/// [`MAX_QUERY_TEXT`] bytes. See [`Learning::record_launch`] for what the bound
/// is for and which caller it protects.
///
/// The check is against the *normalized* key — the trimmed, lowercased string
/// that actually lands in the map — and against nothing else. Checking the raw
/// text as well would refuse queries whose key would have fit: trimming and
/// lowercasing can both shrink a string (`ẞ` is three bytes and lowercases to
/// two, and `  …  firefox` is mostly whitespace), and a refusal here is
/// permanent and silent, so the exact test is the right one.
fn bounded_query_key(query: &str) -> Option<String> {
    let normalized = query.trim().to_lowercase();
    (normalized.len() <= MAX_QUERY_TEXT).then_some(normalized)
}

/// Entries in `global_frequency` older than the retention cutoff, purged.
/// Split out of `save` so it can be applied to a local clone rather than
/// mutating `self` (see the module docs — `save` takes `&self`).
fn purge_retention(
    global_frequency: &HashMap<String, LearningEntry>,
) -> HashMap<String, LearningEntry> {
    let cutoff = now_ms().saturating_sub(PERSIST_RETENTION_MS);
    let mut purged = global_frequency.clone();
    purged.retain(|_, entry| entry.last_ms >= cutoff);
    purged
}

// --- Learning implementation ---

impl Learning {
    /// An empty state: the current `STORE_VERSION`, no selections, no global
    /// frequency.
    fn empty() -> Self {
        Self {
            version: STORE_VERSION,
            selections: HashMap::new(),
            global_frequency: HashMap::new(),
            plaintext_providers: HashSet::new(),
        }
    }

    /// Load from disk, falling back to an empty state on any error — a
    /// missing file, a path that is not a regular file, more bytes than
    /// `MAX_STORE_BYTES`, unreadable bytes, unparseable bytes, valid JSON of
    /// the wrong shape, or a `version` that is not `STORE_VERSION` all land
    /// here. Never panics.
    ///
    /// Which of those happened is discarded here rather than unavailable:
    /// [`Learning::load_reporting`] does the work and returns a
    /// [`LoadReport`] beside the store, and this is that function with the
    /// report dropped. A caller that only wants a usable store keeps this
    /// one-value signature; a caller that wants to know why it is empty calls
    /// the sibling. There is one implementation of the load rules, so the two
    /// cannot drift apart.
    ///
    /// Everything below is the behavior both entry points share.
    ///
    /// # The two things a store says about itself
    ///
    /// A file asserts what no parse can check: which format its bytes are in
    /// (`version`), and when each entry was last launched (`last_ms`). Both
    /// used to be taken at face value, and they are dealt with differently
    /// because they are different claims.
    ///
    /// The version is checked before either parse below, by reading it and
    /// nothing else out of the document (`StoreVersionProbe`), and a mismatch
    /// refuses the whole store — see `STORE_VERSION` for why refusing beats
    /// reinterpreting, and what refusing costs a user who downgrades.
    ///
    /// Issue #38 checked it inside both parse branches instead, which was
    /// weaker than it read. Both branches deserialize a document completely
    /// before its `version` field is reachable, so the check only ever ran on
    /// documents that already parsed as *this* version's shape — and a version
    /// is bumped because the shape changed. A store from a later hop that
    /// moved anything failed both parses and was reported as damaged, which is
    /// the opposite of what the check was for. Reading the version first
    /// depends on no shape at all, so it covers every document that says which
    /// format it is, and it replaces the two per-branch checks rather than
    /// joining them: one outcome decided at one site cannot come to disagree
    /// with itself.
    ///
    /// The timestamps are not checked but corrected: `purge_and_bound`
    /// clamps a `last_ms` ahead of the load instant back to it, touching no
    /// stamp that is not, and says there why a clamp rather than a refusal
    /// and what the clamp does and does not buy.
    ///
    /// # Limits that do not subsume one another
    ///
    /// Three apply to a load, and no one of them implies another:
    /// `MAX_STORE_BYTES` bounds how many *bytes* are read,
    /// `MAX_GLOBAL_ENTRIES` how many *entries* survive, and [`MAX_ITEM_ID`]
    /// how long a surviving entry's *key* may be. A store can sit far under
    /// the byte ceiling and still hold a hundred thousand entries; a store
    /// can hold a single entry and be gigabytes of whitespace; and a store
    /// can be legal on both of those counts with a megabyte key in it.
    /// `read_bounded_store` applies the byte ceiling, alongside its own
    /// separate guard on what *kind* of thing the path is — which is not a
    /// size limit at all — and `purge_and_bound` applies the other two after
    /// the parse, in the order its own doc comment justifies.
    ///
    /// The cap is applied to what was actually parsed, and how many entries
    /// a file holds is never taken on trust: `MAX_GLOBAL_ENTRIES` used to be
    /// enforced only by `record`, so a store that *arrived* over the cap
    /// stayed over it until the next launch was recorded — `record` has
    /// always ended by evicting down to the cap, so a session that records
    /// one is repaired by it. A session that records none never is, and that
    /// is an ordinary session: reading boosts and saving touches neither
    /// `record` nor, before this change, any cap at all, and
    /// [`Learning::save`] applies none of its own (see `MAX_STORE_BYTES` for
    /// what that makes this the load-path half of). `purge_expired` is no
    /// substitute either — it drops entries by age, so it keeps every entry
    /// whose timestamp is recent, including one the clamp has just moved
    /// from the future to the load instant, however many of those there are.
    ///
    /// Of this module's own three size caps, `MAX_GLOBAL_ENTRIES` is the only
    /// one with anything to do here. `MAX_QUERIES` and `MAX_ITEMS_PER_QUERY`
    /// bound the per-query selections, which no load keeps: `save` never
    /// writes them, and both branches below discard whatever a file offered
    /// in their place. The other limit applied below, [`MAX_ITEM_ID`], is
    /// `hop-protocol`'s and bounds a key's length rather than any count.
    ///
    /// # What the entry cap bounds, and what it does not
    ///
    /// It bounds *how many* entries survive a load. It does not decide
    /// *which*: `evict_lru_map` evicts by oldest `last_ms`, so the newest
    /// stamp in the map is the last entry dropped. Future-dating never let
    /// an entry evade the cap — the count came down regardless — but it did
    /// let one win eviction against real learning, by an unbounded margin
    /// and for ever.
    ///
    /// The clamp bounds that margin without settling the question. No entry
    /// now reaches eviction stamped later than the load instant, where one
    /// could previously carry a date no clock will reach. What the clamp
    /// does not do is make the honest entries win: every entry already on
    /// disk was stamped before the load, so a clamped entry is still the
    /// newest in the map and still the last evicted. What that costs is one
    /// slot out of `MAX_GLOBAL_ENTRIES`; what it no longer costs is that
    /// slot against every launch to come, since a launch recorded in this
    /// session is stamped after the load instant, which puts the clamped
    /// entry ahead of it in the eviction order rather than behind.
    ///
    /// Settling it means refusing the entry, and refusing needs a reason to
    /// believe the rest of the store — an integrity check, which is issue
    /// #38's out-of-scope half and is implemented nowhere here. The module
    /// docs say what that leaves standing.
    pub fn load(path: &Path) -> Learning {
        Self::load_reporting(path).0
    }

    /// [`Learning::load`], plus a [`LoadReport`] saying what it noticed: that
    /// the store loaded, or which single condition sent it back empty. This is
    /// where both entry points' load rules actually live, and `load` is this
    /// function with the report dropped — see [`Learning::load`] for the rules
    /// themselves, and [`LoadReport`] for why the report rides beside the
    /// store rather than replacing it with a `Result`.
    ///
    /// The store this returns is a usable one in every case, and the report
    /// never changes it. A caller is free to ignore the report entirely, which
    /// is precisely what `load` does.
    ///
    /// # Reporting is all this does
    ///
    /// It decides nothing on the strength of what it found. Nothing here logs,
    /// counts, retries, quarantines the file it could not read, or treats one
    /// report as more serious than another — what the daemon should do with a
    /// report is deliberately left to whoever grows the first caller (issue
    /// #43 produces the channel and stops there). In particular a report is
    /// not a promise that the file is still there, or still says the same
    /// thing, by the time it is read: it describes one load that has already
    /// finished.
    ///
    /// # What the report cannot say
    ///
    /// [`LoadReport::Loaded`] is not a statement that the store is
    /// trustworthy, only that it passed the guards this module applies.
    /// [`LoadReport`] says what that leaves undetected, and why the missing
    /// half is issue #88 rather than something this function could add.
    pub fn load_reporting(path: &Path) -> (Learning, LoadReport) {
        let data = match read_bounded_store(path) {
            Ok(data) => data,
            Err(report) => return (Self::empty(), report),
        };
        // The version is read on its own, before either full parse, so that a
        // store announcing a format this code does not understand is reported
        // as that whatever else its shape turns out to be. See
        // `StoreVersionProbe`.
        let Ok(probe) = serde_json::from_str::<StoreVersionProbe>(&data) else {
            return (Self::empty(), LoadReport::Malformed);
        };
        if probe.version != STORE_VERSION {
            return (
                Self::empty(),
                LoadReport::UnrecognizedVersion {
                    found: probe.version,
                },
            );
        }
        if let Ok(persisted) = serde_json::from_str::<PersistedLearningStore>(&data) {
            let mut store = Self::empty();
            store.global_frequency = persisted.global_frequency;
            store.purge_and_bound();
            return (store, LoadReport::Loaded);
        }
        if let Ok(mut store) = serde_json::from_str::<Learning>(&data) {
            store.selections.clear();
            store.purge_and_bound();
            return (store, LoadReport::Loaded);
        }
        (Self::empty(), LoadReport::Malformed)
    }

    /// Everything a freshly parsed store owes before [`Learning::load`] hands
    /// it out: drop entries whose key is over [`MAX_ITEM_ID`] bytes, re-key
    /// what survives through [`persistence_key`] (merging any collisions),
    /// clamp every `last_ms` in the future back to now, drop entries that
    /// have expired, then evict whatever is still over `MAX_GLOBAL_ENTRIES`.
    ///
    /// The key bound and the clamp apply to `global_frequency` alone,
    /// because it is the only half of a [`Learning`] a load populates:
    /// `save` never writes `selections`, and of `load`'s two branches one
    /// never assigns them and the other clears them. `purge_expired` does
    /// sweep both, being shared with `record`; at load its pass over
    /// `selections` has nothing to do.
    ///
    /// # Why the key bound is applied here at all
    ///
    /// `global_frequency` is a `HashMap<String, _>`, so parsing one imposes
    /// no length on its keys. [`provider_scoped_key`] bounds a key built on
    /// the record path — it can only ever be as long as a legal provider and
    /// a legal id-part allow — and bounds nothing at all about a key that
    /// arrived off disk, since none was built on the way in. A key over the
    /// bound therefore cannot be learning this module recorded, and is
    /// dropped rather than kept: keeping it would break the byte ceiling's
    /// own derivation, which assumes every persisted key is at most
    /// [`MAX_PERSISTED_KEY_LEN`] bytes — see `MAX_STORE_BYTES`, where that
    /// assumption is stated and this is the enforcement it names. The bound
    /// checked here is `MAX_PERSISTED_KEY_LEN`, not the narrower
    /// [`MAX_ITEM_ID`] issue #37 introduced this check with: a
    /// provider-scoped key legitimately carries the provider's own bytes on
    /// top of the id-part, so a maximal genuine key now runs past
    /// `MAX_ITEM_ID` — checking that narrower bound here would drop this
    /// module's own freshly recorded, maximally-long entries on their very
    /// next load, which is exactly the restart-survival failure this issue's
    /// brief rules out.
    ///
    /// The entry is dropped, not the store around it. That follows the
    /// policy `saturating_count_i32`'s doc comment sets out for this module:
    /// degrade bad data rather than discard the good alongside it.
    ///
    /// This is a bound — how long a key may be — and not a rule about what a
    /// key may *contain*. The id-scrubbing rule is the latter, is a separate
    /// issue, and is deliberately not applied here; the two are easy to
    /// mistake for one another because both look like "checking the id".
    ///
    /// # Why re-keying happens immediately after the bound, and not before it
    ///
    /// [`rekeyed_global_frequency`] maps every surviving key through
    /// [`rekeyed_legacy_key`] — the load-path half of the persistence-key
    /// rule, whose module docs are on [`persistence_key`] itself. It runs
    /// second, straight after the length check above and nowhere else.
    /// Unlike before issue #72, the load-time migration no longer has a
    /// branch that *shrinks* a key — option A never hashes a legacy id on
    /// the way in, it either leaves an already-scoped key exactly as long as
    /// it was, re-attributes an `app:` key to a longer, provider-scoped one,
    /// or drops the entry outright — so there is no laundering route left
    /// where checking the bound after re-keying would let over-long garbage
    /// launder itself into a short-looking entry the way an unconditional
    /// hash once could. The ordering is kept anyway, for a reason that still
    /// holds: it is what keeps the bound anchored to what a store actually
    /// *claims*, rather than to some transformation of it, and it means an
    /// inadmissible key is never handed to the migration step's own work at
    /// all.
    ///
    /// Re-keying can turn two distinct surviving keys into one — two `app:`
    /// keys re-attributed to the same provider, say — and
    /// [`rekeyed_global_frequency`] merges rather than lets the second
    /// overwrite the first: counts sum (saturating) and `last_ms` takes the
    /// later of the two. Overwriting would discard real learning one of the
    /// two source entries recorded; see [`rekeyed_global_frequency`]'s own
    /// doc comment for why summing and taking the later stamp are each the
    /// right merge for their field.
    ///
    ///
    /// # Why the future stamps are clamped rather than trusted or dropped
    ///
    /// `last_ms` is a claim the file makes about when an entry was last
    /// launched, and nothing else in the store corroborates it. A stamp
    /// ahead of the clock is one no honest `record` wrote — `record` stamps
    /// `now_ms()` — so it is either a clock that has since moved backwards
    /// or a number somebody chose, and from here the two look identical.
    ///
    /// Trusting it is what this replaces, and the cost of trusting it was
    /// not a slightly generous boost: `apply_decay` returns the raw,
    /// undecayed value for as long as `now <= last_ms`, and
    /// `purge_retention` keeps anything at or past the cutoff, so an entry
    /// dated far enough ahead held full boost for ever and could never age
    /// out of the file either.
    ///
    /// Clamping reads the stamp as the most recent honest value it could
    /// have been. Dropping the entry instead is the alternative rejected: a
    /// modest forward skew is ordinary — a store synced from a machine whose
    /// clock runs fast, a VM resumed with a bad RTC — and dropping would
    /// silently delete real learning to punish a clock, against this
    /// module's standing policy of degrading bad data rather than discarding
    /// it (`saturating_count_i32`'s doc comment).
    ///
    /// What the clamp buys is narrower than "the entry is now decayed", and
    /// the difference matters. The clamped stamp is `now`, so the very next
    /// `apply_decay` sees an age of zero, which is inside the half-life, and
    /// returns the raw undecayed value: at the load instant the entry is
    /// boosted exactly as an entry launched a moment ago would be. What
    /// changes is that the boost now *starts ageing* from somewhere, where
    /// before it aged from a date no clock would reach. And even that is
    /// contingent, because the clamp is a load-path guard with no
    /// counterpart in `save`, which writes what memory holds and validates
    /// nothing: a corrected stamp becomes durable only when the store is
    /// next written, and until then every load clamps the file's own value
    /// again, from that session's instant.
    ///
    /// Being stricter would buy nothing. An attacker who can write the file
    /// can write `last_ms` at the current time instead of the far future,
    /// collect the same boost and trip no check here at all — a future stamp
    /// was never their best move. Zeroing or hard-decaying a future entry
    /// would therefore cost them one edit, and cost an honest user with a
    /// skewed clock the learning they really did earn. What the clamp gives
    /// up is stated where it lands: [`Learning::load`] on eviction,
    /// `apply_decay` on the boost itself, and the module docs on the store no
    /// clamp can help with.
    ///
    /// # Why this order
    ///
    /// Each step hands the next a smaller or saner map, and each is about
    /// something different: what is not learning at all, what an honest key
    /// looks like once persisted, what cannot be true, what is too old to
    /// count, and what is simply too much. The key bound goes first so
    /// neither re-keying nor eviction ever has to treat an inadmissible key
    /// as a real one — re-keying would otherwise turn an over-long key into
    /// a short, legitimate-looking hash (the paragraph above works through
    /// why), and eviction would otherwise spend a slot deciding between a
    /// genuine entry and one that was never admissible, exactly the future-
    /// dated-key hazard the paragraph below describes for the clamp.
    ///
    /// Re-keying goes second, before the clamp, though the two turn out not
    /// to interact: [`rekeyed_global_frequency`]'s merge takes the later of
    /// two `last_ms` values, and `min(a, now)` against `max(a, b)` commutes
    /// with `min(b, now)` against the same `now` — clamping each source
    /// entry first and then taking the later of the two clamped stamps gives
    /// the same result as merging first and clamping the merged stamp
    /// after. Placed before the clamp anyway, because a step that decides
    /// *which* entries exist reads better ahead of a step that only
    /// corrects a field on the entries re-keying already settled.
    ///
    /// The clamp's position, by contrast, is free against *both* of the
    /// steps that follow it, and saying so is better than implying an order
    /// that does work it does not do.
    ///
    /// Against `evict_lru_map`: eviction drops the smallest `last_ms`, and
    /// the clamp is `min(last_ms, now)`, which is monotonic — `a <= b`
    /// implies `min(a, now) <= min(b, now)`. A monotonic rewrite can never
    /// invert a comparison, only flatten a strict one into a tie, and the
    /// only stamps it moves are those above `now`. So every entry stamped
    /// behind the load instant survives, or does not, identically on either
    /// side of the eviction.
    ///
    /// Two comparisons the order can change, neither of them a matter of
    /// honesty. Which of two *future-dated* entries is dropped: clamped they
    /// tie at `now` and `HashMap` iteration order picks, unclamped the
    /// nearer-future one goes first. And an entry stamped on the load
    /// instant exactly, which ties with a clamped entry rather than losing
    /// to it by the millisecond it was written earlier.
    ///
    /// Against `purge_expired`: the purge keeps entries at or after the
    /// retention cutoff, and a future stamp and the `now` that replaces it
    /// are both past that cutoff, so no entry changes side either way.
    ///
    /// It is written where it is because a correction reads better before
    /// the two steps that select — not because either of them would decide
    /// differently. What the clamp does *not* do is fix eviction's
    /// preference for a future-dated entry; [`Learning::load`] works through
    /// why, and it is why that preference is not listed among this issue's
    /// achievements.
    ///
    /// Purging before evicting is the key bound's argument once more: it
    /// leaves eviction only entries that were going to be kept anyway, where
    /// the reverse order would spend evictions on entries the purge was
    /// about to drop and leave a needlessly smaller store behind.
    fn purge_and_bound(&mut self) {
        self.global_frequency
            .retain(|id, _| id.len() <= MAX_PERSISTED_KEY_LEN);
        self.global_frequency = rekeyed_global_frequency(&self.global_frequency);
        let now = now_ms();
        for entry in self.global_frequency.values_mut() {
            entry.last_ms = entry.last_ms.min(now);
        }
        self.purge_expired();
        evict_lru_map(&mut self.global_frequency, MAX_GLOBAL_ENTRIES);
    }

    /// Persist to disk via a temp file + atomic rename + directory fsync,
    /// mode 0600. Creates the parent directory if it doesn't exist yet, at
    /// mode 0700 on unix —
    /// but a parent that already exists is left exactly as found, whatever
    /// its mode. `persist_atomically`'s `DirBuilder` block says why that
    /// asymmetry is load-bearing.
    ///
    /// Per-query selections are never written — only the retention-purged
    /// global frequency table is.
    ///
    /// The keys written are exactly the keys `global_frequency` holds in
    /// memory, unmodified. That is not an oversight: every entry reached
    /// this table either through [`Learning::record`], which inserts under
    /// [`persistence_key`] rather than the raw id, or through a load, which
    /// re-keys through the same function in [`Learning::purge_and_bound`] —
    /// see [`rekeyed_global_frequency`] for why that has to happen on the
    /// way in rather than here on the way out. So by the time any caller
    /// reaches this function, the map is already keyed the way it will be
    /// written; a canonicalizing pass here would either repeat work
    /// [`Learning::record`] and [`Learning::load`] already did or, for an
    /// id neither of those built this value from — a `Learning` an outside
    /// caller deserialized straight from JSON and never loaded (see
    /// `MAX_STORE_BYTES`'s doc comment on that route) — silently launder an
    /// out-of-bound key this function has no bound to check it against.
    ///
    /// The version written is `STORE_VERSION`, never `self.version`: these
    /// bytes are in the format this function serializes, whatever version
    /// the value in memory happens to carry. That distinction is not
    /// academic — [`Learning`] derives `Default`, which zeroes `version`, so
    /// copying the field through would have any caller that started from
    /// `Learning::default()` rather than from a file (`Pipeline`'s own field
    /// among them) write a store the very next [`Learning::load`] refuses.
    ///
    /// Nothing about `last_ms` is corrected on the way out. `save` writes
    /// what memory holds; the clamp belongs to `purge_and_bound`, where
    /// untrusted bytes arrive.
    ///
    /// A store that cannot be serialized returns `Err` having created
    /// nothing at all — no file, no directory — so whatever is already on
    /// disk survives intact. `serialize_and_persist` is where that ordering
    /// lives.
    ///
    /// # A save destroys the file a load reported on
    ///
    /// That protection covers a serialization failure and nothing else. A
    /// save that *succeeds* replaces whatever was at `path`, and this
    /// function neither reads the destination first nor takes a
    /// [`LoadReport`], so it cannot know it is about to overwrite the store a
    /// load just called [`LoadReport::Malformed`] or
    /// [`LoadReport::UnrecognizedVersion`]. The rename is atomic, so the
    /// original is not corrupted — it is gone, along with any chance of
    /// examining what happened to it, and the copy hop later downgraded away
    /// from goes the same way as one a disk damaged.
    ///
    /// Preserving or quarantining that file is deliberately not done here.
    /// Issue #43 gave the load path a reporting channel and named this as the
    /// half it was not fixing, so this paragraph is the record of a known gap
    /// rather than a description of a guard: nothing below defends the
    /// original, and a caller that wants one kept must copy it aside itself,
    /// between the load that reported and the save that overwrites.
    ///
    /// This is the only other entry point (besides `load`) that touches the
    /// filesystem.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let purged_global = purge_retention(&self.global_frequency);
        serialize_and_persist(
            path,
            &PersistedLearningStore {
                version: STORE_VERSION,
                global_frequency: purged_global,
            },
        )
    }

    /// Replaces the set of provider ids this store currently treats as safe
    /// to persist in the clear, wholesale, with `ids` — [`persistence_key`]'s
    /// `persist_plaintext` argument, for whichever provider [`Learning::record`]
    /// or [`Learning::frequency_boost`] next asks about, is read straight off
    /// this set (`self.plaintext_providers.contains(provider)`).
    ///
    /// # Why this exists, and why `Learning` does not compute it itself
    ///
    /// [`persistence_key`]'s plaintext-versus-hash decision is the
    /// manifest's alone now (issue #72's Composition decision), and a
    /// [`ProviderManifest`](crate::provider::ProviderManifest) is not
    /// something this module holds or knows how to read — `Learning` is
    /// `hop-core`'s persistence layer, not its provider registry, and this
    /// method keeps it that way on purpose. What it takes is the *answer* to
    /// "which ids has a manifest already vouched for", computed by whoever
    /// does hold the registry — `hopd`'s daemon wiring, at startup, from
    /// `ProviderHost::manifests()` — and handed over as plain ids, so this
    /// module needs nothing about `ProviderManifest`'s other fields (its
    /// `kinds`, its `budget`, ...) to use the answer.
    ///
    /// # Fail-closed by construction
    ///
    /// A provider id absent from `ids` is `false`/hashed the moment `record`
    /// or `frequency_boost` next asks — there is no third state here, only
    /// "in the set" or "not". A provider that never registered is never in
    /// `ids` in the first place, so it can never be granted plaintext
    /// persistence by omission: this method has nothing to consult for it,
    /// and neither does whoever built `ids`.
    ///
    /// # Not persisted, deliberately
    ///
    /// The field this sets, `plaintext_providers`, is `#[serde(skip)]` — see
    /// its own doc comment for why a store loaded from disk must never be
    /// able to grant itself plaintext persistence for the reads and writes
    /// that follow. [`Learning::load`] therefore always returns a value with
    /// this set empty, hashing everything, until whoever holds the live
    /// store calls this method with the real one.
    ///
    /// # Wholesale replacement, not a merge
    ///
    /// Set once, from the whole current registry, rather than grown
    /// incrementally: a provider that stops declaring the flag — or stops
    /// registering at all — must lose plaintext persistence the next time
    /// this is called, not keep an entry a caller forgot to remove. That
    /// loss is not just a matter of future writes hashing instead of not —
    /// see the next paragraph for what it does to what is already on disk.
    ///
    /// # Revocation reaches what is already stored, not only what is next recorded
    ///
    /// Replacing the set alone would leave a gap: a provider that persisted
    /// ids in the clear while opted in, then flips
    /// `ids_are_safe_to_persist_in_the_clear` to `false`, would have every id
    /// it already wrote sit on disk in the clear for the rest of
    /// `PERSIST_RETENTION_MS` (90 days) regardless — [`Learning::load`]'s
    /// legacy-shape migration and this module's own round-trip both leave an
    /// already-provider-scoped key exactly as they found it, without asking
    /// whether its id-part still matches what the provider *currently*
    /// claims. This call is where that gets checked, because it is the
    /// moment the registry's answer changes: every `global_frequency` entry
    /// whose provider is no longer in the set just installed, and whose
    /// id-part is not already a hash, is re-hashed on the spot, carrying its
    /// count and `last_ms` over to the new key under the same merge
    /// [`rekeyed_global_frequency`] uses. See
    /// [`Learning::rehash_entries_for_providers_no_longer_opted_in`] for the
    /// mechanics, what it assumes about `ids`, and the one direction this
    /// cannot fix (a hash cannot be turned back into the plaintext it came
    /// from, for a provider that opts in *later*).
    pub fn sync_plaintext_providers(&mut self, ids: impl IntoIterator<Item = String>) {
        self.plaintext_providers = ids.into_iter().collect();
        self.rehash_entries_for_providers_no_longer_opted_in();
    }

    /// [`Learning::sync_plaintext_providers`]'s revocation half: re-hashes
    /// every `global_frequency` entry whose provider is not in
    /// `self.plaintext_providers` (just replaced by the caller) but whose
    /// stored id-part is still plaintext.
    ///
    /// # Only one direction is fixable
    ///
    /// A plaintext id-part re-hashes to `sha256:` plus the digest of that
    /// exact id-part — precisely what [`persistence_key`] would compute for
    /// the same raw id under `persist_plaintext: false`, so a future lookup
    /// for the now-unopted provider finds it under the new key, with its
    /// count intact. The reverse can never happen, and this method does not
    /// attempt it: a hash-shaped id-part for a provider that has since opted
    /// *in* is left exactly as it is, because a SHA-256 digest is one-way —
    /// there is no raw id here to recover and write back in the clear. Those
    /// entries simply age out of `PERSIST_RETENTION_MS` on their own, the
    /// same as any other hashed entry; nothing here accelerates or delays
    /// that. This asymmetry is a property of hashing, not a gap in this
    /// method.
    ///
    /// # Why this runs here, and not in `Learning::load`
    ///
    /// `load` cannot make this call: it runs before this method is ever
    /// invoked — `hopd`'s daemon wiring calls
    /// [`Learning::sync_plaintext_providers`] *after* `Learning::load`, once
    /// the registry is available (see that method's own doc comment) — so at
    /// load time there is no registry answer yet to check a stored key's
    /// provider against, only the empty default every load starts from. The
    /// moment the answer *does* arrive is this call, so this is where the
    /// check happens: on every sync, against whichever set was just
    /// installed.
    ///
    /// # `ids` is assumed to be the complete, authoritative registry
    ///
    /// This treats "provider absent from the just-installed set" as
    /// "currently not opted in" — there is no third answer available to it,
    /// and none is invented: [`Learning::sync_plaintext_providers`]'s own
    /// contract already requires callers to pass the complete current
    /// registry, never a partial one (`plaintext_provider_ids(&host.manifests())`,
    /// which captures every registered provider at once — see
    /// `hop_core::provider::plaintext_provider_ids`). A caller that violates
    /// that contract — passing a set missing a provider that is genuinely
    /// still opted in, rather than one that has revoked or never
    /// registered — gets that provider's entries re-hashed exactly as if it
    /// had revoked, because nothing at this layer can tell the two apart
    /// from the set alone; there is no flag carried alongside `ids` saying
    /// "this is everyone."
    ///
    /// What this method does *not* do is treat a `Learning` that has never
    /// been synced at all as though every provider had revoked. That state —
    /// between `Learning::load` and the first call to
    /// [`Learning::sync_plaintext_providers`] — never reaches this method,
    /// because this method only runs *from inside* a sync call. There is no
    /// standing "run on every access" version of this check that could catch
    /// a `Learning` before its first sync in a comparison against an
    /// as-yet-nonexistent answer; the check exists only at the instant an
    /// answer is supplied.
    ///
    /// # The one gap this cannot close
    ///
    /// A raw id can, in principle, already look exactly like this module's
    /// own hash output (`sha256:` plus 64 lowercase hex characters) —
    /// [`persistence_key`]'s own doc comment discusses this for the record
    /// path, where it is harmless as long as the provider stays opted in.
    /// If such an id was written in the clear by a provider that has since
    /// revoked, [`looks_like_a_persistence_hash`] cannot distinguish it from
    /// an entry that was already a genuine hash, and this method leaves it
    /// as it found it. A narrow, pre-existing ambiguity this module has
    /// always accepted, not a new one this method introduces — see
    /// [`persistence_key`]'s doc comment, "Why the plaintext/hash partition
    /// is still provable with a provider folded in".
    fn rehash_entries_for_providers_no_longer_opted_in(&mut self) {
        let mut migrations = Vec::new();
        for (key, entry) in &self.global_frequency {
            let Some((provider, id_part)) = parse_provider_scoped_key(key) else {
                // Not this module's own key shape at all — nothing for this
                // pass to do; `rekeyed_legacy_key` (load-time only) is what
                // handles a legacy shape, not this method.
                continue;
            };
            if self.plaintext_providers.contains(provider) || looks_like_a_persistence_hash(id_part)
            {
                continue;
            }
            let hashed_id_part = format!("sha256:{:x}", Sha256::digest(id_part.as_bytes()));
            let new_key = provider_scoped_key(provider, &hashed_id_part);
            migrations.push((key.clone(), new_key, entry.clone()));
        }
        for (old_key, new_key, entry) in migrations {
            self.global_frequency.remove(&old_key);
            merge_learning_entry(&mut self.global_frequency, new_key, &entry);
        }
    }

    /// Record a launch: the user reached `item_id`, produced by `provider`,
    /// while typing `query`.
    ///
    /// `provider` is folded into both keys this call touches
    /// ([`provider_scoped_key`], via [`Learning::record`]) so that a launch
    /// recorded for one provider's id can never be read back as another
    /// provider's — issue #72's fix for the boost-theft gap issue #39 left
    /// open. It is not itself validated here; whatever the caller passes is
    /// what gets folded in, on the same trust footing `query` and `item_id`
    /// already have.
    ///
    /// The launch always counts toward `item_id`'s global launch frequency.
    /// It is additionally learned *against this query* only if the query's
    /// normalized form — trimmed and lowercased, which is the key it would be
    /// stored under — is at most [`MAX_QUERY_TEXT`] bytes. A longer one is
    /// refused, never truncated: a shortened query is a different query, and
    /// would collect launches that were never made under it.
    ///
    /// # The wire bound does not subsume this one
    ///
    /// `ClientMsg::Query.text` carries the same constant at `hop-protocol`'s
    /// deserialization boundary (issue #22), but the two checks measure
    /// different strings: the wire counts the raw bytes that arrived, this one
    /// counts the normalized key and nothing else. Normalization can *grow* a
    /// key past a length the wire already accepted — `İ` (U+0130) is two bytes
    /// and lowercases to three, so 512 of them are 1 024 raw bytes, exactly on
    /// the wire bound, and normalize to a 1 536-byte key that is refused here.
    /// A wire-legal query can therefore reach this check and be refused by it,
    /// silently dropping its per-query learning; only the global launch count
    /// below survives. The test
    /// `a_query_whose_key_grows_past_the_bound_when_normalized_is_refused` is
    /// that counterexample, spelled out.
    ///
    /// The bound is also the *only* one for the other caller: `hop-core` is a
    /// library, and something that builds a [`Learning`] and calls this method
    /// directly never crossed the wire at all, so no upstream check of any kind
    /// ran. `MAX_QUERIES` is no help either way, because it bounds how *many*
    /// query keys the map holds, never how large one is.
    ///
    /// # What it does not bound
    ///
    /// Storage only. It is not an allocation guard, and this crate does not
    /// have one: the key is normalized (allocating a lowercased copy) before
    /// its size is known, and [`Learning::boost_for`] normalizes the query
    /// again on every lookup with no bound at all — which
    /// `Pipeline::assemble` invokes once per candidate item, so an over-long
    /// term costs a lowercased copy *per item* on that keystroke. Refusing to
    /// store the key does nothing about any of that. A caller that must bound
    /// the memory a query can cost has to bound the query itself, upstream, as
    /// the wire boundary does.
    pub fn record_launch(&mut self, provider: &str, query: &str, item_id: &ItemId) {
        self.record(provider, query, item_id.as_str());
    }

    /// Record a selection: the user chose `result_id`, produced by
    /// `provider`, while typing `query`.
    ///
    /// `selections` and `global_frequency` key `result_id` differently, and
    /// that split is deliberate rather than an inconsistency to fix.
    /// `selections` is in-memory only — `save` never writes it — so nothing
    /// about issue #39's persistence-key rule applies to it, and its inner
    /// key is [`provider_scoped_key`] applied to the *raw* `result_id`, with
    /// no hashing decision on top: [`Learning::query_boost`] recomputes the
    /// same scoped key before its own lookup. `global_frequency` is exactly
    /// what does get written, so it is inserted under the full
    /// [`persistence_key`] here rather than under `result_id` itself — the
    /// entry point where an id enters the store, which is what
    /// [`Learning::frequency_boost`] has to compute the same key at on the
    /// way back out. Hashing only where `save` writes, and leaving
    /// `global_frequency` keyed by raw id in memory, was the alternative
    /// rejected: a reload would then key the map by hash while every lookup
    /// still keyed by raw id, silently breaking a hashed provider's learning
    /// across a restart. See [`persistence_key`] and [`provider_scoped_key`]
    /// for the rules themselves.
    fn record(&mut self, provider: &str, query: &str, result_id: &str) {
        self.purge_expired();
        let ts = now_ms();

        // Update per-query selections, unless the key would be over its bound
        // — see `Learning::record_launch` for what that bound is and is not.
        // The launch is still counted globally below: that table is keyed by
        // the item id, which `ItemId::new` bounds separately, so refusing it
        // too would discard a real signal over a hazard belonging to this
        // table alone.
        if let Some(normalized) = bounded_query_key(query) {
            let inner = self.selections.entry(normalized).or_default();
            let entry = inner
                .entry(provider_scoped_key(provider, result_id))
                .or_insert(LearningEntry {
                    count: 0,
                    last_ms: 0,
                });
            entry.count = entry.count.saturating_add(1);
            entry.last_ms = ts;

            // Evict inner map if too large
            evict_lru_map(inner, MAX_ITEMS_PER_QUERY);

            // Evict outer map if too large
            evict_lru_outer(&mut self.selections, MAX_QUERIES);
        }

        // Update global frequency, keyed by the persistence key rather than
        // the raw id — see this function's doc comment for why here rather
        // than at `save`. `persist_plaintext` reads whichever answer
        // `sync_plaintext_providers` last gave for `provider`; a provider
        // never synced (never registered, or registered after the last
        // sync) is not in the set and defaults to `false` — hashed.
        let persist_plaintext = self.plaintext_providers.contains(provider);
        let global = self
            .global_frequency
            .entry(persistence_key(provider, result_id, persist_plaintext))
            .or_insert(LearningEntry {
                count: 0,
                last_ms: 0,
            });
        global.count = global.count.saturating_add(1);
        global.last_ms = ts;

        // Evict global map if too large
        evict_lru_map(&mut self.global_frequency, MAX_GLOBAL_ENTRIES);
    }

    /// Clear all learned data. Unlike the salvage's `reset`, this does not
    /// persist — `Learning` no longer owns a path to persist to. Callers
    /// that want the clear to survive a restart must call `save` themselves.
    pub fn reset(&mut self) {
        self.selections.clear();
        self.global_frequency.clear();
    }

    /// Compute a query-specific boost for `result_id`, produced by
    /// `provider`.
    ///
    /// Prefix matching works both ways:
    /// - A shorter stored key that is a prefix of `query` contributes.
    /// - A longer stored key that starts with `query` also contributes.
    ///
    /// The boost is count * QUERY_BOOST_PER_COUNT, with recency decay,
    /// clamped to `0..=QUERY_BOOST_CAP`. Non-negative is a property of this
    /// function itself, not of `boost_for`'s later clamp: `count` is
    /// converted via [`saturating_count_i32`] rather than a bare `as i32`
    /// (see its doc comment for why), and the result below is a `.clamp`
    /// rather than a `.min` so that floor is enforced explicitly instead of
    /// resting on every step above happening to stay non-negative.
    ///
    /// `provider` is folded into the lookup key with [`provider_scoped_key`]
    /// before any stored inner map is consulted — issue #72's fix for this
    /// half of the boost-theft gap: before it, `evil` presenting
    /// `app:firefox` matched exactly the entries `apps` had earned on the
    /// same id, because the inner map was keyed on the bare id alone.
    fn query_boost(&self, provider: &str, query: &str, result_id: &str) -> i32 {
        let normalized = query.trim().to_lowercase();
        if normalized.is_empty() {
            return 0;
        }
        let now = now_ms();
        let mut total: i32 = 0;
        let scoped_id = provider_scoped_key(provider, result_id);

        for (stored_query, inner) in &self.selections {
            // Prefix match: either stored_query is a prefix of the current query
            // or the current query is a prefix of the stored_query.
            let is_prefix_match = normalized.starts_with(stored_query.as_str())
                || stored_query.starts_with(&normalized);
            if !is_prefix_match {
                continue;
            }
            if let Some(entry) = inner.get(&scoped_id) {
                let raw = saturating_count_i32(entry.count).saturating_mul(QUERY_BOOST_PER_COUNT);
                total = total.saturating_add(apply_decay(raw, entry.last_ms, now));
            }
        }

        total.clamp(0, QUERY_BOOST_CAP)
    }

    /// Compute a global frequency boost for `result_id`, produced by
    /// `provider`, with recency decay, clamped to `0..=FREQ_BOOST_CAP` — the
    /// same non-negative-and-capped property as [`Learning::query_boost`],
    /// for the same reason; see its doc comment.
    ///
    /// `result_id` is the raw id — the same one a caller would pass to
    /// [`Learning::query_boost`] — and not `global_frequency`'s own key
    /// space. This function computes [`persistence_key`] over `provider` and
    /// it before looking up, the read-side half of the same rule
    /// [`Learning::record`]'s doc comment explains from the write side: the
    /// map is keyed by persistence key in memory as well as on disk, so a
    /// lookup that skipped this step would silently miss every entry whose
    /// raw id and persistence key differ — everything this rule hashes — and
    /// a lookup that dropped `provider` from that key, the same way, would
    /// miss nothing: it would instead find *any* provider's entry for the
    /// same raw id, which is issue #72's boost-theft gap exactly.
    fn frequency_boost(&self, provider: &str, result_id: &str) -> i32 {
        let now = now_ms();
        let persist_plaintext = self.plaintext_providers.contains(provider);
        if let Some(entry) =
            self.global_frequency
                .get(&persistence_key(provider, result_id, persist_plaintext))
        {
            let raw = saturating_count_i32(entry.count).saturating_mul(FREQ_BOOST_PER_COUNT);
            apply_decay(raw, entry.last_ms, now).clamp(0, FREQ_BOOST_CAP)
        } else {
            0
        }
    }

    /// The learned boost for this provider/query/item combination: the sum
    /// of `query_boost` and `frequency_boost`, clamped to
    /// `0.0..=LEARNING_BOOST_CAP`. This is the value the ranker consumes.
    ///
    /// `provider` must be the provider that actually produced `item_id` on
    /// this query — `Pipeline::assemble` passes `item.provider`, the field
    /// [`crate::pipeline::CheckedItems::check`] has already held to the
    /// producing provider's own manifest. Passing a different provider here
    /// answers honestly for *that* provider's history on `item_id`, which is
    /// zero unless it too has launches recorded under the pairing — it does
    /// not "look up `item_id` under whichever provider has a boost for it";
    /// that conflation is exactly the vulnerability issue #72 closes.
    pub fn boost_for(&self, provider: &str, query: &str, item_id: &ItemId) -> f32 {
        let total = self.query_boost(provider, query, item_id.as_str())
            + self.frequency_boost(provider, item_id.as_str());
        (total as f32).clamp(0.0, LEARNING_BOOST_CAP)
    }

    /// Return the most recently launched result IDs, sorted by last_ms descending.
    pub fn recent_launches(&self, limit: usize) -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = self
            .global_frequency
            .iter()
            .map(|(id, entry)| (id.clone(), entry.last_ms))
            .collect();
        entries.sort_by_key(|(_, last_ms)| std::cmp::Reverse(*last_ms));
        entries.truncate(limit);
        entries
    }

    /// Return the most frequently launched result IDs, sorted by count descending,
    /// excluding the given IDs.
    pub fn frequent_launches(&self, limit: usize, exclude: &[String]) -> Vec<(String, u32)> {
        let mut entries: Vec<(String, u32)> = self
            .global_frequency
            .iter()
            .filter(|(id, _)| !exclude.contains(id))
            .map(|(id, entry)| (id.clone(), entry.count))
            .collect();
        entries.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        entries.truncate(limit);
        entries
    }

    /// Returns true if there are no selections and no global frequency entries.
    pub fn is_empty(&self) -> bool {
        self.selections.is_empty() && self.global_frequency.is_empty()
    }

    fn purge_expired(&mut self) {
        let cutoff = now_ms().saturating_sub(PERSIST_RETENTION_MS);
        self.selections.retain(|_, inner| {
            inner.retain(|_, entry| entry.last_ms >= cutoff);
            !inner.is_empty()
        });
        self.global_frequency
            .retain(|_, entry| entry.last_ms >= cutoff);
    }
}

/// The bytes at `path`, or the [`LoadReport`] for why there are none.
/// [`Learning::load`]'s only way to reach the filesystem.
///
/// Four of the seven reports are decided here — [`LoadReport::Absent`],
/// [`LoadReport::Unreadable`], [`LoadReport::NotARegularFile`] and
/// [`LoadReport::TooLarge`] — and the mapping from guard to report is not
/// one-to-one in either direction. `LoadReport::from_io` splits *one* guard,
/// the stat, into two outcomes on `io::ErrorKind::NotFound`. And three sites
/// can produce [`LoadReport::Unreadable`]: the stat, the open, and the UTF-8
/// decode. The open is the one worth spelling out, because it is the same
/// absent-against-unreadable pair once more: a `NotFound` there means the file
/// was removed between the stat and the open, so it runs through `from_io`
/// like any other I/O error and correctly reports `Absent`, not `Unreadable`.
///
/// All four were one `None` until issue #43, which is why this returns a
/// `Result` over a report rather than an [`Option`] — the caller cannot
/// recover the distinction once it is gone, and there is nowhere else it
/// survives.
///
/// The remaining three ([`LoadReport::Loaded`], [`LoadReport::Malformed`],
/// [`LoadReport::UnrecognizedVersion`]) are the parse's to decide, so
/// [`Learning::load_reporting`] owns those. Nothing here reads the bytes for
/// anything but their length and their encoding.
///
/// `CONTEXT.md` scopes **bound** to a length rule on a wire value, living in
/// `hop-protocol`'s `limits`; the word is used here in that same sense —
/// a length, checked at a deserialization boundary — one boundary over, on a
/// file rather than a frame, and not as some second meaning.
///
/// # Two guards and a decode, in this order
///
/// **The stat comes first, and must.** Opening a FIFO for reading blocks
/// until a writer appears — forever, for a store nobody is writing to — so
/// the refusal has to happen before the open, not after it. A size check
/// could not stand in for this either: a FIFO and a character device both
/// report a length of zero, so by length alone `/dev/zero` is an empty
/// store.
///
/// [`fs::metadata`] follows symlinks, which is what is wanted here. The
/// check is on what the path *resolves to*: a symlink to `/dev/zero`
/// resolves to a character device and is refused, while a symlink to a
/// regular file resolves to a regular file and loads — pointing
/// `~/.local/state/hop` at another volume is an ordinary thing to do and
/// must keep working.
///
/// **The read is bounded, not merely pre-checked.** `metadata.len()` is a
/// hint and not a guarantee: a character device reports zero whatever it
/// would yield, and an ordinary file can grow between the stat and the read.
/// So the length from the stat is never consulted; [`std::io::Read::take`]
/// bounds the read itself, and at most `MAX_STORE_BYTES + 1` bytes are ever
/// read, whatever the stat said or did not say. The claim is about bytes
/// read rather than bytes allocated: the `Vec<u8>` they land in is grown by
/// `read_to_end` and may hold spare capacity past its length, which is a
/// constant factor on a bounded number, not an unbounded one. The `String`
/// this returns adds nothing to that — `String::from_utf8` takes the vector
/// by value and keeps its buffer rather than copying it.
///
/// The `+ 1` is what distinguishes a store sitting exactly on the
/// ceiling — legitimate, and loaded — from one over it: at the ceiling the
/// read returns `MAX_STORE_BYTES` bytes and stops of its own accord, and
/// over it the extra byte comes back and the store is refused.
///
/// **Measured, then decoded — in that order.** The read is over bytes, and
/// the ceiling is applied to them before any decode, because `take` cuts at a
/// byte offset and can land inside a multibyte character. Decoding first made
/// an over-size store whose character straddles the cut fail as invalid UTF-8,
/// reporting [`LoadReport::Unreadable`] for a file that was refused for its
/// size and was valid UTF-8 from end to end. Size does not need the bytes
/// decoded to measure, so it is not measured on decoded bytes.
///
/// # The window between them
///
/// The two calls name a path, not an object, so between the stat and the
/// open the path can be replaced — the classic TOCTOU window. What that can
/// cost is a load that blocks (a FIFO swapped in after the stat is opened
/// without complaint) or that reads a device's bytes instead of a file's.
/// What it cannot cost is unbounded memory: `take` bounds whatever the open
/// landed on, so the byte ceiling holds even when the stat check is
/// defeated. That is the division of labour between the two guards — the
/// stat decides *what kind of thing* is read, the ceiling decides *how much*
/// — and it is why neither one substitutes for the other.
///
/// Closing the window itself means opening first and re-checking the
/// descriptor (`O_NONBLOCK` so a FIFO cannot block the open, then `fstat` on
/// the file that was actually opened). That is a unix-specific open path for
/// a race an attacker who can write the store's directory has better uses
/// for: writing a large regular file, which the ceiling refuses without any
/// racing at all. It is left out deliberately rather than overlooked.
fn read_bounded_store(path: &Path) -> Result<String, LoadReport> {
    let metadata = fs::metadata(path).map_err(|err| LoadReport::from_io(&err))?;
    if !metadata.is_file() {
        return Err(LoadReport::NotARegularFile);
    }

    let mut data = Vec::new();
    fs::File::open(path)
        .map_err(|err| LoadReport::from_io(&err))?
        .take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|err| LoadReport::from_io(&err))?;

    // The ceiling is checked on the bytes, before they are decoded, because
    // size is a property of the bytes and needs nothing decoded to measure.
    // Decoding first got this wrong in the one case it most needed to get
    // right: `take` cuts at a byte offset, so an over-size store whose
    // multibyte character straddles the cut comes back as invalid UTF-8 even
    // though the file is valid throughout, and the decode failed before the
    // ceiling was ever consulted — reporting `Unreadable` for a store that was
    // refused for its size alone.
    if data.len() as u64 > MAX_STORE_BYTES {
        return Err(LoadReport::TooLarge);
    }
    // `InvalidData` names exactly what `read_to_string` reports for bytes that
    // are not UTF-8, so a genuinely undecodable store keeps the report it had.
    // Past the ceiling check, a decode failure is now that and only that.
    String::from_utf8(data).map_err(|_| LoadReport::Unreadable(io::ErrorKind::InvalidData))
}

/// Serialize `value`, then persist it — and only in that order. A value
/// that fails to serialize returns `Err` before `persist_atomically` runs,
/// so nothing on disk is touched: not the store, not even its parent
/// directory. `persist_atomically` takes a `&str`, so there is no way to
/// reach the filesystem without a payload that already serialized.
///
/// Generic over `T` rather than taking `&PersistedLearningStore` so this
/// stays the seam the failure is tested through. The persisted shape is
/// `String` keys over `u32`/`u64` today, which `to_string_pretty` cannot
/// fail on, so no test could provoke the failure through
/// [`Learning::save`]. Handing this a value that genuinely fails to
/// serialize exercises the real ordering, and keeps the guard honest for
/// the day the persisted shape grows a field that can fail.
fn serialize_and_persist<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    // `InvalidData` because that is what this is from the filesystem's point
    // of view: data that cannot be represented on disk. The
    // `unwrap_or_default()` that used to stand here turned the same
    // condition into an empty payload, wrote it over a good store, and
    // reported success.
    let payload = serde_json::to_string_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    persist_atomically(path, &payload)
}

/// The filesystem half of [`Learning::save`]: create the parent directory
/// if it is missing, write `payload` to a temp file beside the destination,
/// rename it into place, sync the containing directory so that rename
/// survives a crash, and narrow the result to 0600 on unix.
///
/// Layered over [`write_and_rename`], which is only the temp-file
/// creation-through-rename half of the sequence and owns that temp file's
/// cleanup on failure; this is the function that owns the surrounding
/// directory and destination-permission steps — the directory sync belongs
/// here, alongside the `mkdir` above, for the same reason: both are about
/// the directory `write_and_rename`'s temp file happens to sit in, not
/// about that temp file itself.
fn persist_atomically(path: &Path, payload: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        // A directory this code creates is narrowed to 0700; one that was
        // already there is left exactly as found, whatever its mode. That
        // asymmetry is deliberate and load-bearing: `path` is derived from
        // `XDG_STATE_HOME`, which the user controls and which this module
        // never sees, so we can only reason about a directory we created
        // ourselves. A user who exports `XDG_STATE_HOME=$HOME` must not
        // find their home directory silently narrowed to 0700 on first
        // launch, and `Learning::save` is the only persistence entry point,
        // so there would be no way to opt out.
        //
        // `DirBuilder` rather than `create_dir_all` + `set_permissions`,
        // for three reasons that all come down to only ever touching a
        // directory we made:
        //
        // - The mode is an argument to `mkdir(2)`, so the directory is
        //   born at 0700. There is no window at a wider mode to race,
        //   unlike a create-then-chmod pair. `mkdir` masks the mode with
        //   the umask (`mode & ~umask`), which can only clear bits — and
        //   0700 has no group or other bits to begin with — so no umask
        //   can widen this, only narrow it to something useless-but-safe.
        // - `recursive(true)` makes an existing path a no-op rather than
        //   an error: std's `mkdir` returns `EEXIST`, std confirms the
        //   path is a directory, and stops. Both syscalls are read-only
        //   with respect to the mode, so a pre-existing parent keeps
        //   whatever it had by construction — not by a check we could
        //   forget to write.
        // - That also disposes of symlinks. `chmod(2)` follows them and
        //   would have narrowed a symlinked parent's *target*; `mkdir(2)`
        //   on a symlink to a directory just returns `EEXIST`.
        //
        // Recursive creation applies 0700 to every component it creates,
        // not only the leaf — if `~/.local/state` is missing too, it is
        // created at 0700 as well. That is correct under the same rule:
        // we created it, so it is ours to narrow. Anything that already
        // existed is skipped, so the reach stops at the first component
        // we did not make.
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(parent)?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no valid file name")
        })?;

    write_and_rename(parent, file_name, path, payload.as_bytes())?;

    // Sync the directory immediately after the rename it is making durable,
    // and before the chmod below. The chmod mutates the destination file's
    // inode, not the directory entry `fs::rename` just changed, so it has
    // no bearing on what this sync guarantees either way — ordering it
    // first just keeps the directory's one call next to the one operation
    // it is about, rather than splitting them across an unrelated step.
    #[cfg(unix)]
    sync_parent_directory(parent)?;

    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Open the directory at `parent` and fsync it. `fs::rename` mutates the
/// directory's entry, not the file it moves — [`write_and_rename`]'s
/// `file.sync_all()` already makes the file's own contents durable, but
/// leaves the rename itself resting on the filesystem's own commit
/// interval. This closes that gap: once this returns `Ok`, the rename
/// [`persist_atomically`] just performed is durable too.
///
/// unix-only, gated the same way every other unix-specific step in this
/// module is (`DirBuilderExt`, `OpenOptionsExt`, `PermissionsExt`, the 0600
/// chmod above): `File::open` succeeds on a directory on unix but not on
/// Windows, where a directory cannot be opened as a file at all. The gate
/// reflects that platform difference in how to even attempt this, not a
/// decision that directory durability stops mattering off unix.
#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

/// Create a temp file beside `dest` (see [`create_temp_file_exclusive`]),
/// write `payload` to it, fsync it, then rename it onto `dest`. Split out of
/// [`persist_atomically`] purely to keep the `?`-propagation linear.
///
/// Owns cleanup of the temp file for the whole of its own lifetime: once
/// [`create_temp_file_exclusive`] has picked a name and created it, this is
/// the only function that knows which name that was, so a failure in the
/// write, fsync or rename below removes that exact path before returning. A
/// collision during creation itself needs no such cleanup — nothing was
/// created — which is why that case is handled entirely inside
/// [`create_temp_file_exclusive`] instead.
fn write_and_rename(parent: &Path, file_name: &str, dest: &Path, payload: &[u8]) -> io::Result<()> {
    let (temp_path, mut file) =
        create_temp_file_exclusive(parent, temp_file_candidates(file_name))?;

    let result = (|| -> io::Result<()> {
        file.write_all(payload)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp_path, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

/// How many fresh names [`create_temp_file_exclusive`] will try before
/// giving up. A collision means something already sits at the name an
/// attempt picked; each name mixes the pid, a nanosecond clock reading and
/// the attempt index (see [`temp_file_name`]), so two temp-file creations
/// racing each other collide only if all three happen to match — in
/// practice, the same process retrying with the same pid against a clock
/// that read the same nanosecond twice. One retry already recovers from
/// that; 5 leaves headroom for it to happen a few times over (a coarse
/// clock on some platform, say) without turning a transient collision into
/// a hard failure, while still giving up long before it could look like a
/// hang.
const MAX_TEMP_FILE_ATTEMPTS: u32 = 5;

/// The exact name sequence [`write_and_rename`] offers
/// [`create_temp_file_exclusive`]: one [`temp_file_name`] per attempt, for
/// attempts `0..MAX_TEMP_FILE_ATTEMPTS`. Named and tested on its own so that
/// wiring — offering the documented number of attempts, each genuinely
/// distinct from the last — is pinned directly, rather than resting on
/// [`write_and_rename`] still doing so correctly being read off a `map`
/// call embedded in a longer function.
fn temp_file_candidates(file_name: &str) -> impl Iterator<Item = String> + '_ {
    (0..MAX_TEMP_FILE_ATTEMPTS).map(move |attempt| temp_file_name(file_name, attempt))
}

/// Build the `attempt`th candidate temp-file name for `file_name`. The pid
/// and the clock reading carry only on-disk diagnostic value, if a leftover
/// temp file is ever found; neither is what makes two names in the same
/// retry sequence distinct. Nothing forces the clock to have ticked forward
/// between two attempts nanoseconds apart, so re-reading it alone would not
/// guarantee that — `attempt` is threaded through as an explicit,
/// strictly-increasing component so distinctness holds by construction
/// instead. See [`create_temp_file_exclusive`] for where that distinctness
/// matters.
fn temp_file_name(file_name: &str, attempt: u32) -> String {
    format!(
        ".{}.tmp-{}-{}-{}",
        file_name,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        attempt
    )
}

/// Try each name in `candidate_names`, in order, as a temp file inside
/// `parent`, opened with `create_new` (`O_EXCL`) so that anything already at
/// the path — a pre-existing file *or* a symlink, which `create_new` refuses
/// to open through regardless of what it points at — fails the open instead
/// of being written to. Only that specific failure (`AlreadyExists`) moves
/// on to the next name; any other error returns immediately, since a fresh
/// name would not fix a permission error or a full disk.
///
/// The retry bound lives entirely in how many names `candidate_names`
/// yields — see [`temp_file_candidates`] for where production draws that
/// line and what makes successive names distinct. If
/// every name collides, the last collision is returned as-is: it already
/// accurately describes what went wrong, and synthesizing a "gave up after
/// N attempts" error in its place would only discard that.
///
/// Extracted to take `parent` plus a name sequence, rather than being
/// folded into [`write_and_rename`], specifically so a test can drive the
/// real retry loop with names it controls. Production's names come from the
/// pid and the clock, which a test cannot predict precisely enough to
/// pre-plant a collision at the exact path the next save will pick — the
/// same problem [`serialize_and_persist`]'s doc comment solves for
/// serialization failures, solved here by making the input the seam instead
/// of the timing.
fn create_temp_file_exclusive(
    parent: &Path,
    candidate_names: impl Iterator<Item = String>,
) -> io::Result<(PathBuf, fs::File)> {
    let mut last_collision = None;
    for name in candidate_names {
        let candidate = parent.join(name);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no temp file candidate names were supplied",
        )
    }))
}

// --- Tests ---

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // --- Ported from the salvage, behavior unmodified. ---

    #[test]
    fn record_and_query_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning_test_nonexistent.json");
        let mut store = Learning::load(&path);

        store.record(APPS_PROVIDER_ID, "fire", "app:firefox");
        store.record(APPS_PROVIDER_ID, "fire", "app:firefox");
        store.record(APPS_PROVIDER_ID, "fire", "app:firewall");
        store.record(APPS_PROVIDER_ID, "code", "app:vscode");

        // firefox was selected twice for "fire"
        let inner = store.selections.get("fire").unwrap();
        assert_eq!(
            inner
                .get(&provider_scoped_key(APPS_PROVIDER_ID, "app:firefox"))
                .unwrap()
                .count,
            2
        );
        assert_eq!(
            inner
                .get(&provider_scoped_key(APPS_PROVIDER_ID, "app:firewall"))
                .unwrap()
                .count,
            1
        );

        // global frequency — `false` throughout: this test is about
        // recording and lookup agreeing on one key, not about which side of
        // the plaintext/hash partition that key falls on, and `store` was
        // never synced with `sync_plaintext_providers`, so `false` is what
        // `record`'s own internal decision for `APPS_PROVIDER_ID` actually
        // is here.
        assert_eq!(
            store
                .global_frequency
                .get(&persistence_key(APPS_PROVIDER_ID, "app:firefox", false))
                .unwrap()
                .count,
            2
        );
        assert_eq!(
            store
                .global_frequency
                .get(&persistence_key(APPS_PROVIDER_ID, "app:firewall", false))
                .unwrap()
                .count,
            1
        );
        assert_eq!(
            store
                .global_frequency
                .get(&persistence_key(APPS_PROVIDER_ID, "app:vscode", false))
                .unwrap()
                .count,
            1
        );

        // query_boost should be positive for a matching query/result pair
        let boost = store.query_boost(APPS_PROVIDER_ID, "fire", "app:firefox");
        assert!(boost > 0, "expected positive boost, got {boost}");

        // frequency_boost should be positive
        let freq = store.frequency_boost(APPS_PROVIDER_ID, "app:firefox");
        assert!(freq > 0, "expected positive freq boost, got {freq}");
    }

    #[test]
    fn save_and_load_round_trip_without_persisting_query_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut store = Learning::load(&path);
        store.record(APPS_PROVIDER_ID, "fire", "app:firefox");
        store.record(APPS_PROVIDER_ID, "fire", "app:firefox");
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains("\"fire\""),
            "raw query keys should not be persisted"
        );

        let loaded = Learning::load(&path);
        assert_eq!(
            loaded
                .global_frequency
                // `false`: this store was never synced, so this is what its
                // own `record` call hashed `"app:firefox"` under too.
                .get(&persistence_key(APPS_PROVIDER_ID, "app:firefox", false))
                .unwrap()
                .count,
            2
        );
        assert!(
            loaded.selections.is_empty(),
            "query selections should remain in-memory only after reload"
        );
    }

    // `canonicalizes_dynamic_result_ids_for_persistence` used to live here,
    // pinning issue #39's shape rule stripping a payload off `utility:`/
    // `web-search:` ids before persisting them in the clear. Issue #72
    // removed that stripping along with the shape rule itself — the
    // manifest is the sole authority now, and an opted-in provider's raw id
    // persists verbatim, unstripped — so there is nothing left for that test
    // to pin. See `an_opted_in_providers_id_persists_in_the_clear_regardless_of_shape`
    // below for its replacement.

    #[test]
    fn empty_store_returns_no_boosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning_test_empty_nonexistent.json");
        let store = Learning::load(&path);

        assert!(store.is_empty());
        assert_eq!(
            store.query_boost(APPS_PROVIDER_ID, "anything", "app:foo"),
            0
        );
        assert_eq!(store.frequency_boost(APPS_PROVIDER_ID, "app:foo"), 0);
        assert!(store.recent_launches(10).is_empty());
        assert!(store.frequent_launches(10, &[]).is_empty());
    }

    #[test]
    fn lru_eviction_respects_max_queries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        for i in 0..510 {
            store.record(APPS_PROVIDER_ID, &format!("query{i}"), "some.desktop");
        }
        assert!(
            store.selections.len() <= 500,
            "selections should be capped at MAX_QUERIES"
        );
    }

    #[test]
    fn prefix_matching_boosts_across_query_lengths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        store.record(APPS_PROVIDER_ID, "firefox", "firefox.desktop");
        store.record(APPS_PROVIDER_ID, "firefox", "firefox.desktop");
        store.record(APPS_PROVIDER_ID, "firefox", "firefox.desktop");

        // "fi" should match "firefox" via prefix matching
        let boost = store.query_boost(APPS_PROVIDER_ID, "fi", "firefox.desktop");
        assert!(boost > 0, "prefix 'fi' should match learning for 'firefox'");

        // "firefox browser" should match "firefox" too (starts_with)
        let boost2 = store.query_boost(APPS_PROVIDER_ID, "firefox browser", "firefox.desktop");
        assert!(boost2 > 0, "longer query should match stored shorter key");
    }

    #[test]
    fn recent_launches_sorted_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        // This test is about `recent_launches`' sort order, not about the
        // plaintext/hash partition, so `false` throughout (unsynced store)
        // is as good a key as any — what matters is that `record` and the
        // assertion below compute the same one.
        store.record(APPS_PROVIDER_ID, "a", "app:first");
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.record(APPS_PROVIDER_ID, "b", "app:second");

        let recent = store.recent_launches(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(
            recent[0].0,
            persistence_key(APPS_PROVIDER_ID, "app:second", false),
            "most recent should be first"
        );
        assert_eq!(
            recent[1].0,
            persistence_key(APPS_PROVIDER_ID, "app:first", false)
        );
    }

    #[test]
    fn frequent_launches_excludes_specified_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        // This test is about exclusion, which needs the id it excludes by to
        // be the id `global_frequency` is actually keyed by; the
        // plaintext/hash partition itself is not what is under test here, so
        // `false` (this store is never synced) is fine.
        for _ in 0..5 {
            store.record(APPS_PROVIDER_ID, "a", "app:popular");
        }
        for _ in 0..2 {
            store.record(APPS_PROVIDER_ID, "b", "app:other");
        }

        let popular_key = persistence_key(APPS_PROVIDER_ID, "app:popular", false);
        let frequent = store.frequent_launches(10, std::slice::from_ref(&popular_key));
        assert!(
            frequent.iter().all(|(id, _)| *id != popular_key),
            "excluded IDs should not appear"
        );
        assert!(!frequent.is_empty(), "should still have other entries");
    }

    // Adapted per decision 7: the salvage's `reset` cleared state *and*
    // persisted, using the path it stored on construction. `Learning` no
    // longer owns a path, so `reset` only clears in-memory state now; the
    // persistence half is asserted separately here via an explicit
    // save/reload.
    #[test]
    fn reset_clears_all_data_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        store.record(APPS_PROVIDER_ID, "test", "app.desktop");
        assert!(!store.is_empty());

        store.reset();
        assert!(store.is_empty());

        // The salvage's `reset` self-persisted; this one only clears
        // in-memory state, since `Learning` no longer owns a path to
        // persist to. Assert the persistence half explicitly instead.
        store.save(&path).unwrap();
        let loaded = Learning::load(&path);
        assert!(loaded.is_empty());
    }

    // --- New tests from the brief, written failing first. ---

    #[test]
    fn boost_capped_below_alias_boost() {
        let mut l = Learning::load(Path::new("/nonexistent"));
        for _ in 0..10_000 {
            l.record_launch(
                APPS_PROVIDER_ID,
                "fire",
                &ItemId::new("app:firefox").unwrap(),
            );
        }
        let b = l.boost_for(
            APPS_PROVIDER_ID,
            "fire",
            &ItemId::new("app:firefox").unwrap(),
        );
        assert!(b <= LEARNING_BOOST_CAP && b > 0.0);
        assert!(b < 180.0, "alias boost (180) must always beat learning");
    }

    #[test]
    fn corrupt_state_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("learning.json");
        std::fs::write(&p, "{not json").unwrap();
        let l = Learning::load(&p);
        assert_eq!(
            l.boost_for(APPS_PROVIDER_ID, "x", &ItemId::new("y").unwrap()),
            0.0
        );
    }

    #[test]
    fn save_is_atomic_and_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("learning.json");
        let mut l = Learning::load(&p);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        l.save(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            Learning::load(&p).boost_for(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap())
                > 0.0
        );
    }

    // --- Coverage neither source reaches. ---

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("never-written.json");
        let l = Learning::load(&p);
        assert!(l.is_empty());
        assert_eq!(
            l.boost_for(APPS_PROVIDER_ID, "anything", &ItemId::new("app:x").unwrap()),
            0.0
        );
    }

    #[test]
    fn wrong_shaped_json_loads_empty() {
        let dir = tempfile::tempdir().unwrap();

        let array_path = dir.path().join("array.json");
        std::fs::write(&array_path, "[1,2,3]").unwrap();
        let l = Learning::load(&array_path);
        assert!(l.is_empty());

        let bad_version_path = dir.path().join("bad-version.json");
        std::fs::write(&bad_version_path, r#"{"version":"not-a-number"}"#).unwrap();
        let l = Learning::load(&bad_version_path);
        assert!(l.is_empty());
    }

    #[test]
    fn temp_file_does_not_survive_a_successful_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        l.save(&path).unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![path.file_name().unwrap().to_owned()]);
    }

    #[test]
    fn save_over_existing_file_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut first = Learning::load(&path);
        first.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        first.save(&path).unwrap();

        // Load fresh, wipe it, and record something entirely different. If
        // `save` appended rather than replaced the file, the reload below
        // would still carry "app:a"'s entry alongside the new one.
        let mut second = Learning::load(&path);
        second.reset();
        second.record_launch(APPS_PROVIDER_ID, "other", &ItemId::new("app:b").unwrap());
        second.save(&path).unwrap();

        let reloaded = Learning::load(&path);
        assert_eq!(
            reloaded.frequency_boost(APPS_PROVIDER_ID, "app:b"),
            second.frequency_boost(APPS_PROVIDER_ID, "app:b"),
            "reloaded state should match the saver's"
        );
        assert_eq!(
            reloaded.frequency_boost(APPS_PROVIDER_ID, "app:a"),
            0,
            "the first save's data should not survive being replaced by the second"
        );
    }

    // --- Unvalidated persisted count (issue #44). ---
    //
    // `query_boost` and `frequency_boost` each convert `entry.count`
    // through `saturating_count_i32` — see its doc comment for why, and for
    // the `i32::MAX` ceiling `rekeyed_global_frequency` shares. The tests
    // below pin three independent properties: a deserialization boundary
    // that saturates a persisted count on the way in, each boost function
    // staying correct on its own for *any* `LearningEntry` (including one
    // built in memory that never touched the boundary at all), and that
    // same ceiling holding when a load merges two colliding entries.

    // Pins the deserialization boundary itself: if
    // `#[serde(deserialize_with = "deserialize_saturating_count")]` were
    // removed, `serde_json` still parses `4000000000` into `count: u32`
    // unchanged — nothing about deserializing a `u32` wraps — and both
    // boost functions would go on saturating it themselves regardless (see
    // `loading_a_store_with_an_out_of_range_count_produces_a_non_negative_capped_frequency_boost`
    // below), so no boost-level assertion could ever catch the attribute
    // being removed. Deserializing a `LearningEntry` directly and reading
    // `count` back is what can.
    #[test]
    fn deserializing_an_out_of_range_count_saturates_it_to_i32_max() {
        let entry: LearningEntry =
            serde_json::from_str(r#"{"count":4000000000,"last_ms":0}"#).unwrap();
        assert_eq!(entry.count, i32::MAX as u32);
    }

    // `query_boost` considered on its own: an entry built directly in
    // memory, bypassing `LearningEntry`'s deserialization boundary
    // entirely, so this fails unless `query_boost` itself is safe — it
    // cannot be passing only because some upstream boundary happened to
    // saturate the count first.
    #[test]
    fn query_boost_is_non_negative_and_capped_for_an_in_memory_out_of_range_count() {
        let mut l = Learning::empty();
        l.selections.insert(
            "fire".to_string(),
            HashMap::from([(
                provider_scoped_key(APPS_PROVIDER_ID, "app:firefox"),
                LearningEntry {
                    count: u32::MAX,
                    last_ms: now_ms(),
                },
            )]),
        );

        let boost = l.query_boost(APPS_PROVIDER_ID, "fire", "app:firefox");
        assert!(
            boost >= 0,
            "query_boost must never go negative, got {boost}"
        );
        assert_eq!(boost, QUERY_BOOST_CAP);
    }

    // `frequency_boost` considered on its own, same shape as the test
    // above. `boost_for` is never called here, so this cannot be passing
    // because of its caller's clamp.
    #[test]
    fn frequency_boost_is_non_negative_and_capped_for_an_in_memory_out_of_range_count() {
        let mut l = Learning::empty();
        l.global_frequency.insert(
            persistence_key(APPS_PROVIDER_ID, "app:firefox", false),
            LearningEntry {
                count: u32::MAX,
                last_ms: now_ms(),
            },
        );

        let boost = l.frequency_boost(APPS_PROVIDER_ID, "app:firefox");
        assert!(
            boost >= 0,
            "frequency_boost must never go negative, got {boost}"
        );
        assert_eq!(boost, FREQ_BOOST_CAP);
    }

    // The disk-reachable path end to end: a store *loaded from disk* with a
    // count past `i32::MAX`. Per-query selections are never persisted (see
    // `PersistedLearningStore`'s doc comment), so a real load can only ever
    // hand a bad count to `global_frequency` — this is what makes
    // `frequency_boost` the one reachable through `Learning::load` here.
    // Asserted through `frequency_boost` directly (not `boost_for`), so a
    // regression that reintroduced the caller's clamp as the only guard
    // would still be caught. This stays green even with only one of the two
    // production fixes in place — the deserialization boundary and
    // `frequency_boost`'s own saturation are both independently sufficient
    // for this specific path — which is why the boundary is pinned
    // separately above, and `frequency_boost`'s isolation is pinned
    // separately below.
    #[test]
    fn loading_a_store_with_an_out_of_range_count_produces_a_non_negative_capped_frequency_boost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{"app:firefox":{{"count":4000000000,"last_ms":18446744073709551615}}}}}}"#
            ),
        )
        .unwrap();

        let mut l = Learning::load(&path);
        // The legacy plaintext `app:` key is re-attributed to the apps
        // provider on load regardless of syncing (`rekeyed_legacy_key`
        // doesn't consult the plaintext set at all); the *lookup* below does,
        // though, and has to agree that "apps" persists in the clear or it
        // recomputes a hashed key that no longer matches the migrated entry.
        l.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        let boost = l.frequency_boost(APPS_PROVIDER_ID, "app:firefox");
        assert!(
            boost >= 0,
            "frequency_boost must never go negative, got {boost}"
        );
        assert_eq!(boost, FREQ_BOOST_CAP);
    }

    // The ceiling has to hold on the way in from a load too, even though
    // issue #72's option A means two *distinct* legacy ids can no longer
    // collide onto one key (see `rekeyed_legacy_key`: the app: branch and the
    // identity branch are each injective on their own). The one collision
    // still reachable is cross-branch: a v1 store predating this issue can
    // hold a plain `app:<rest>` key alongside a key that already happens to
    // be shaped like this module's own provider-scoped output for the same
    // id — a store re-saved by this code, then hand-edited to add the old
    // plaintext form back in, say. Both re-key to the identical final
    // string, so `rekeyed_global_frequency`'s merge still has to hold the
    // saturation ceiling when that happens. Entered directly here, since
    // going through a real `Learning::load` would only add a filesystem
    // round trip without changing what this pins.
    #[test]
    fn rekeying_global_frequency_saturates_a_merged_count_at_i32_max() {
        let already_scoped = provider_scoped_key(APPS_PROVIDER_ID, "app:dup");
        let mut input: HashMap<String, LearningEntry> = HashMap::new();
        input.insert(
            "app:dup".to_string(),
            LearningEntry {
                count: i32::MAX as u32,
                last_ms: 1,
            },
        );
        input.insert(
            already_scoped.clone(),
            LearningEntry {
                count: i32::MAX as u32,
                last_ms: 2,
            },
        );

        let merged = rekeyed_global_frequency(&input);
        let entry = merged.get(&already_scoped).unwrap();
        assert_eq!(
            entry.count,
            i32::MAX as u32,
            "a merged count must not exceed what a later load would saturate it back down to"
        );
    }

    // Ordinary counts must produce exactly the values they did before this
    // fix — pinning concrete numbers here means a regression can't hide
    // behind "still returns something positive".
    #[test]
    fn ordinary_counts_produce_unchanged_query_and_frequency_boosts() {
        let mut l = Learning::empty();
        let now = now_ms();
        l.selections.insert(
            "fire".to_string(),
            HashMap::from([(
                provider_scoped_key(APPS_PROVIDER_ID, "app:firefox"),
                LearningEntry {
                    count: 2,
                    last_ms: now,
                },
            )]),
        );
        l.global_frequency.insert(
            persistence_key(APPS_PROVIDER_ID, "app:firefox", false),
            LearningEntry {
                count: 2,
                last_ms: now,
            },
        );

        assert_eq!(
            l.query_boost(APPS_PROVIDER_ID, "fire", "app:firefox"),
            2 * QUERY_BOOST_PER_COUNT
        );
        assert_eq!(
            l.frequency_boost(APPS_PROVIDER_ID, "app:firefox"),
            2 * FREQ_BOOST_PER_COUNT
        );
    }

    #[test]
    fn boost_for_never_recorded_pairing_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());

        // A pairing that was never recorded: "app:never-seen" has no
        // query-specific or global-frequency history at all.
        assert_eq!(
            l.boost_for(
                APPS_PROVIDER_ID,
                "q",
                &ItemId::new("app:never-seen").unwrap()
            ),
            0.0
        );
    }

    // `query_boost` early-returns zero for an empty (post-trim) query, but
    // `boost_for` sums in `frequency_boost`, which is query-independent by
    // design — it only looks up the item's global launch history, never the
    // query text. So an item *with* recorded launches, queried with an
    // empty string, returns its frequency boost rather than zero. This is
    // the real interaction the pipeline will meet in M1.7, when an empty
    // term returns everything ranked by weight and boost.
    #[test]
    fn boost_for_empty_query_still_carries_frequency_boost_for_a_recorded_item() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());

        let boost = l.boost_for(APPS_PROVIDER_ID, "", &ItemId::new("app:a").unwrap());
        assert!(
            boost > 0.0,
            "frequency_boost doesn't consider the query text, so a recorded \
             item's boost survives an empty query"
        );
        assert_eq!(
            boost,
            l.frequency_boost(APPS_PROVIDER_ID, "app:a") as f32,
            "with an empty query, boost_for is exactly the frequency component"
        );
    }

    // --- The bound on a recorded query key (issue #22). ---
    //
    // `selections` is keyed by query text, and `MAX_QUERIES` bounds how many
    // keys there are, not how large one is. These pin the size bound on the key
    // itself — see `Learning::record_launch` for how it relates to the wire
    // bound on `ClientMsg::Query.text`, which caller each one protects, and
    // what this one deliberately does not bound.
    //
    // The bound is measured on the normalized key and on nothing else, so the
    // last two below are as load-bearing as the first two: they are what keeps
    // a cheaper raw-text pre-check from being reintroduced, since such a check
    // passes the refusal tests while silently refusing queries whose key would
    // have fit.

    #[test]
    fn an_over_long_query_does_not_become_a_selection_key() {
        let mut l = Learning::empty();
        let query = "a".repeat(MAX_QUERY_TEXT + 1);

        l.record_launch(APPS_PROVIDER_ID, &query, &ItemId::new("app:a").unwrap());

        assert!(
            l.selections.is_empty(),
            "a query over the bound must not be stored as a key, not even truncated"
        );
    }

    // The other side of the bound: a query of exactly `MAX_QUERY_TEXT` bytes
    // is recorded as usual. Without this, refusing every query would satisfy
    // the test above.
    #[test]
    fn a_query_exactly_on_the_bound_is_still_recorded() {
        let mut l = Learning::empty();
        let query = "a".repeat(MAX_QUERY_TEXT);

        l.record_launch(APPS_PROVIDER_ID, &query, &ItemId::new("app:a").unwrap());

        assert!(
            l.selections.contains_key(&query),
            "a query exactly on the bound is legitimate and must still be learned"
        );
        assert!(l.query_boost(APPS_PROVIDER_ID, &query, "app:a") > 0);
    }

    // Only the query key is refused. The launch itself still happened, and
    // the table it lands in is keyed by the item id, which `ItemId::new` has
    // already bounded — so there is no reason to discard that half, and doing
    // so would lose real signal over a hazard that concerns the other table.
    #[test]
    fn an_over_long_query_still_counts_the_launch_in_global_frequency() {
        let mut l = Learning::empty();
        let query = "a".repeat(MAX_QUERY_TEXT + 1);

        l.record_launch(APPS_PROVIDER_ID, &query, &ItemId::new("app:a").unwrap());

        assert!(
            l.frequency_boost(APPS_PROVIDER_ID, "app:a") > 0,
            "the launch is real; only the query key is refused"
        );
    }

    // Normalization can push a key *over* the bound: "İ" (U+0130) is two bytes
    // and lowercases to three ("i" plus a combining dot). This query is within
    // the bound as typed and over it once normalized, so a raw-text-only check
    // would store an over-sized key.
    #[test]
    fn a_query_whose_key_grows_past_the_bound_when_normalized_is_refused() {
        let query = "İ".repeat(MAX_QUERY_TEXT / 2);
        assert!(query.len() <= MAX_QUERY_TEXT, "within bound as typed");
        assert!(
            query.trim().to_lowercase().len() > MAX_QUERY_TEXT,
            "but normalizing grows it past the bound"
        );

        let mut l = Learning::empty();
        l.record_launch(APPS_PROVIDER_ID, &query, &ItemId::new("app:a").unwrap());

        assert!(l.selections.is_empty());
    }

    // And normalization can pull a key back *under* it, which is the case a
    // raw-text pre-check gets wrong: trimming discards the padding entirely,
    // so this query's key is seven bytes. Refusing it would drop a perfectly
    // ordinary pasted query — silently, and for good, since nothing retries a
    // launch that was already recorded.
    #[test]
    fn a_query_that_only_trimming_brings_within_the_bound_is_still_recorded() {
        let query = format!("{}firefox", " ".repeat(MAX_QUERY_TEXT * 2));
        assert!(query.len() > MAX_QUERY_TEXT, "over the bound as typed");

        let mut l = Learning::empty();
        l.record_launch(APPS_PROVIDER_ID, &query, &ItemId::new("app:a").unwrap());

        assert!(
            l.selections.contains_key("firefox"),
            "the key is what is bounded, and this key is seven bytes"
        );
    }

    // The same case for shrinking under lowercase rather than under trim, so
    // the rule is pinned as "the normalized key, whatever normalization did to
    // it" rather than as a special case for whitespace. "ẞ" (U+1E9E) is three
    // bytes and lowercases to "ß" (U+00DF), two.
    #[test]
    fn a_query_that_only_lowercasing_brings_within_the_bound_is_still_recorded() {
        let query = "ẞ".repeat(MAX_QUERY_TEXT / 2);
        assert!(query.len() > MAX_QUERY_TEXT, "over the bound as typed");
        let key = query.trim().to_lowercase();
        assert!(key.len() <= MAX_QUERY_TEXT, "but its key fits");

        let mut l = Learning::empty();
        l.record_launch(APPS_PROVIDER_ID, &query, &ItemId::new("app:a").unwrap());

        assert!(l.selections.contains_key(&key));
    }

    // --- Parent-directory permissions (issue #36). ---
    //
    // These tests are about blast radius: `save` may narrow a directory it
    // created itself, and must not touch anything that was already there.
    // `persist_atomically`'s `DirBuilder` block says why.

    // A parent that already exists keeps whatever mode it had. 0755 is chosen
    // because it is both a plausible real-world mode and unmistakably not
    // 0700, so a stray chmod cannot pass this assertion by coincidence.
    #[cfg(unix)]
    #[test]
    fn save_leaves_a_pre_existing_parent_directory_mode_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("already-here");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        let path = parent.join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        l.save(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o755,
            "save must not re-permission a directory it did not create"
        );
    }

    // A parent this code creates is 0700, and so is every ancestor created
    // along the way — `~/.local/state` counts as "a directory this code
    // created" just as much as `~/.local/state/hop` does.
    //
    // There is deliberately no assertion about a transient wider mode: an
    // in-process test cannot observe one, and a timing loop would only
    // pretend to. So this test pins the post-condition only. The absence of
    // a window is structural rather than empirical, and lives at the code
    // that guarantees it — see `persist_atomically`'s comment on the
    // `mkdir(2)` mode argument.
    #[cfg(unix)]
    #[test]
    fn save_creates_missing_parent_directories_at_0700() {
        let dir = tempfile::tempdir().unwrap();
        let intermediate = dir.path().join("state");
        let leaf = intermediate.join("hop");
        let path = leaf.join("learning.json");

        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        l.save(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777,
            0o700,
            "a directory this code created must be owner-only"
        );
        assert_eq!(
            std::fs::metadata(&intermediate)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "an ancestor this code created is equally ours to narrow"
        );
    }

    // Regression guard for the chmod this fix removed: it followed the
    // symlink and narrowed the *target* — a directory somewhere else
    // entirely, which this code certainly did not create.
    #[cfg(unix)]
    #[test]
    fn save_does_not_chmod_through_a_symlinked_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real-directory");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        let link = dir.path().join("link-to-real-directory");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let path = link.join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        l.save(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755,
            "a symlinked parent's target must not be re-permissioned"
        );
    }

    // --- Serialization failure (issue #41). ---

    // A value `serde_json` genuinely refuses: JSON object keys must be
    // strings, and a tuple is not one. `PersistedLearningStore` cannot
    // produce such a value today, which is exactly why these tests enter at
    // `serialize_and_persist` — see its doc comment. Entering there is also
    // what makes them pin the ordering rather than assume it: the serialize
    // and every filesystem step below it are both inside the call.
    fn unserializable_value() -> HashMap<(u8, u8), u8> {
        let mut tuple_keyed = HashMap::new();
        tuple_keyed.insert((1, 2), 3);
        tuple_keyed
    }

    // Read the bytes, not the parsed store: the failure mode being guarded
    // against wrote an empty file, which parses back to an empty store —
    // indistinguishable from a legitimately empty one.
    #[test]
    fn a_serialization_failure_leaves_an_existing_store_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        l.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert!(serialize_and_persist(&path, &unserializable_value()).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a value that cannot be serialized must not reach the store"
        );
    }

    // The path here is two components deep in an empty temp dir, so a
    // stray directory, a temp file or a destination file all show up as
    // something existing that should not. The missing parent directory is
    // the load-bearing part: it is the first syscall a save makes, so a
    // future edit that creates it before serializing turns this red.
    #[test]
    fn a_serialization_failure_creates_no_file_and_no_directory() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("state");
        let path = parent.join("learning.json");

        assert!(serialize_and_persist(&path, &unserializable_value()).is_err());

        assert!(
            !parent.exists(),
            "the parent directory should not be created"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no destination file and no temp file should be left behind"
        );
    }

    #[test]
    fn save_propagates_error_when_parent_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("not-a-directory");
        std::fs::write(&blocking_file, b"i am a file, not a directory").unwrap();
        let path = blocking_file.join("learning.json");

        let l = Learning::load(&path);
        assert!(l.save(&path).is_err());
    }

    // --- Temp file exclusivity (issue #40). ---
    //
    // `create_temp_file_exclusive` is `write_and_rename`'s real
    // open-and-retry step, called directly here — see its doc comment for
    // why entering there, with test-supplied names, is what a test needs.

    // A pre-existing file at the first candidate name must not be written
    // through — `create_new` refuses to open it at all — and the retry must
    // land on the next name and succeed, still at mode 0600.
    #[cfg(unix)]
    #[test]
    fn create_temp_file_exclusive_skips_a_pre_existing_file_and_retries() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("taken"), b"attacker-planted content").unwrap();

        let names = vec!["taken".to_string(), "fresh".to_string()];
        let (path, file) = create_temp_file_exclusive(dir.path(), names.into_iter()).unwrap();

        assert_eq!(path, dir.path().join("fresh"));
        assert_eq!(
            std::fs::read(dir.path().join("taken")).unwrap(),
            b"attacker-planted content",
            "the pre-existing file must not receive the payload"
        );
        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the file created on the retry must still be mode 0600"
        );
    }

    // A symlink at the first candidate name must not be followed —
    // `create_new` fails on it the same way it fails on a plain file — and
    // its target must be left untouched.
    #[cfg(unix)]
    #[test]
    fn create_temp_file_exclusive_does_not_follow_a_symlinked_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real-target");
        std::fs::write(&target, b"do not touch me").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("taken")).unwrap();

        let names = vec!["taken".to_string(), "fresh".to_string()];
        let (path, _file) = create_temp_file_exclusive(dir.path(), names.into_iter()).unwrap();

        assert_eq!(path, dir.path().join("fresh"));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"do not touch me",
            "the symlinked candidate's target must not receive the payload"
        );
    }

    // Once every candidate has collided, the function gives up and returns
    // the collision itself — not a synthesized error — rather than looping
    // forever.
    #[test]
    fn create_temp_file_exclusive_returns_the_collision_when_every_candidate_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"").unwrap();
        std::fs::write(dir.path().join("b"), b"").unwrap();

        let names = vec!["a".to_string(), "b".to_string()];
        let err = create_temp_file_exclusive(dir.path(), names.into_iter()).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    // `temp_file_candidates` is the exact wiring `write_and_rename` uses to
    // feed `create_temp_file_exclusive` — the two tests below pin it
    // directly, rather than trusting the one-line `map` call at its
    // definition to keep threading `MAX_TEMP_FILE_ATTEMPTS` and
    // `temp_file_name` correctly. A regression that shrank the attempt count
    // (hardcoding a single attempt, say) would not be caught by any save
    // exercised end to end, since a save that never collides only ever
    // needs its first candidate.

    // The wiring offers exactly as many names as the documented retry
    // bound, not more and not fewer.
    #[test]
    fn temp_file_candidates_offers_exactly_max_temp_file_attempts_names() {
        let names: Vec<String> = temp_file_candidates("learning.json").collect();
        assert_eq!(names.len(), MAX_TEMP_FILE_ATTEMPTS as usize);
    }

    // A weaker, end-to-end sanity check than the trailing-component test
    // below: confirms the five names actually offered are distinct in
    // practice. On its own this would not reliably catch `attempt` being
    // dropped from `temp_file_name`'s format string, since the clock also
    // varies across the five calls and would usually paper over that —
    // the trailing-component test below is what pins the by-construction
    // guarantee specifically.
    #[test]
    fn temp_file_candidates_are_pairwise_distinct() {
        let names: Vec<String> = temp_file_candidates("learning.json").collect();
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "every attempt must pick a different name"
        );
    }

    // `temp_file_name`'s doc comment claims distinctness holds "by
    // construction": `attempt` is threaded through as an explicit,
    // strictly-increasing component, not left to the clock. Pin that claim
    // itself — each candidate's trailing `-`-delimited component must be
    // its own attempt index — rather than a downstream consequence
    // (distinctness) that the clock could also satisfy on its own. If
    // `attempt` stops being threaded through, the trailing component
    // becomes the nanosecond reading instead: always a many-digit number,
    // never equal to a single-digit attempt index, so this fails on every
    // run rather than only on the runs where the clock doesn't happen to
    // mask the regression.
    #[test]
    fn temp_file_candidates_carry_their_attempt_index_as_the_trailing_component() {
        let names: Vec<String> = temp_file_candidates("learning.json").collect();
        for (attempt, name) in names.iter().enumerate() {
            let trailing = name.rsplit('-').next().unwrap();
            assert_eq!(
                trailing,
                attempt.to_string(),
                "candidate {attempt} must carry its own attempt index as the trailing component, got {name:?}"
            );
        }
    }

    // --- Directory fsync after rename (issue #42). ---
    //
    // A directory fsync has no observable side effect on a successful run,
    // so the first two tests below enter through `sync_parent_directory`
    // directly — see its doc comment for why it is its own function. The
    // third drives a real `save` end to end and is the stronger evidence:
    // it shows a directory-sync failure surfacing as an `Err` from `save`
    // rather than being swallowed.

    #[cfg(unix)]
    #[test]
    fn sync_parent_directory_succeeds_on_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sync_parent_directory(dir.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn sync_parent_directory_returns_err_rather_than_panicking_on_an_unopenable_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(sync_parent_directory(&missing).is_err());
    }

    // --- The bounded load path (issue #37). ---
    //
    // Two different limits, with two different failure modes, neither of
    // which subsumes the other: `MAX_STORE_BYTES` bounds how many *bytes*
    // `load` will read off disk, and `MAX_GLOBAL_ENTRIES` bounds how many
    // *entries* survive the parse. A store can be far under the byte ceiling
    // and still hold a hundred thousand tiny entries; a store can hold one
    // entry and be gigabytes of whitespace.

    /// A parseable one-entry store, padded to exactly `total_bytes` with
    /// whitespace, which JSON ignores between tokens.
    ///
    /// Padding rather than more entries is what keeps the two tests below
    /// about the byte ceiling *alone*: the store inside is a single entry,
    /// nowhere near the entry cap, so whether it loads turns only on whether
    /// its bytes were admitted.
    fn whitespace_padded_store(total_bytes: usize) -> String {
        let store = format!(
            r#"{{"version":{STORE_VERSION},"global_frequency":{{"app:a":{{"count":3,"last_ms":{}}}}}}}"#,
            now_ms()
        );
        let mut padded = String::with_capacity(total_bytes);
        padded.push_str(&store);
        padded.push_str(&" ".repeat(total_bytes - store.len()));
        padded
    }

    /// A parseable store of `entries` entries, all stamped `last_ms`.
    fn store_of_entries(entries: usize, last_ms: u64) -> String {
        let body: Vec<String> = (0..entries)
            .map(|i| format!(r#""app:{i}":{{"count":1,"last_ms":{last_ms}}}"#))
            .collect();
        format!(
            r#"{{"version":{STORE_VERSION},"global_frequency":{{{}}}}}"#,
            body.join(",")
        )
    }

    #[test]
    fn a_store_one_byte_over_the_byte_ceiling_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let padded = whitespace_padded_store(MAX_STORE_BYTES as usize + 1);
        std::fs::write(&path, &padded).unwrap();

        let loaded = Learning::load(&path);

        assert!(
            loaded.is_empty(),
            "a store over the byte ceiling must be refused, not parsed"
        );
    }

    // The other side of the ceiling. Without this, refusing every store
    // would satisfy the test above — and a store that is exactly as large as
    // the ceiling allows is a store `save` is entitled to have written.
    #[test]
    fn a_store_exactly_on_the_byte_ceiling_is_still_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let padded = whitespace_padded_store(MAX_STORE_BYTES as usize);
        std::fs::write(&path, &padded).unwrap();

        let mut loaded = Learning::load(&path);
        // The fixture's legacy `app:a` key re-attributes to the apps
        // provider on load regardless; the lookup below needs to agree that
        // provider persists in the clear to recompute the same key.
        loaded.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);

        assert!(
            loaded.frequency_boost(APPS_PROVIDER_ID, "app:a") > 0,
            "a store exactly on the byte ceiling is legitimate and must still load"
        );
    }

    // Neither the byte ceiling nor the entry cap can reach this case: one
    // over-long key breaks no count and, on its own, no ceiling either. A
    // store hand-written in compact JSON can sit under the ceiling with a
    // key far past `MAX_PERSISTED_KEY_LEN` in it, load, and then be
    // re-serialized by `save` with `to_string_pretty`'s indentation on
    // top — over the ceiling, unreadable by the very next load.
    // `MAX_PERSISTED_KEY_LEN`, not the narrower `MAX_ITEM_ID`, is the bound
    // enforced here as of issue #72: a provider-scoped key legitimately runs
    // past `MAX_ITEM_ID` now, so checking the old, narrower bound would drop
    // this module's own genuine maximal output — see that constant's doc
    // comment.
    //
    // The bound is checked against `over_long` as the file wrote it, before
    // `rekeyed_global_frequency` ever runs — `Learning::purge_and_bound`'s
    // doc comment says why that ordering is kept even though, unlike before
    // issue #72, re-keying can no longer shrink a key at all.
    #[test]
    fn a_persisted_key_over_the_bound_is_dropped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let over_long = "a".repeat(MAX_PERSISTED_KEY_LEN + 1);
        let now = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{"{over_long}":{{"count":9,"last_ms":{now}}},"app:a":{{"count":1,"last_ms":{now}}}}}}}"#
            ),
        )
        .unwrap();

        let loaded = Learning::load(&path);

        assert!(
            !loaded.global_frequency.contains_key(&over_long),
            "a key over MAX_PERSISTED_KEY_LEN must not survive a load"
        );
        assert!(
            loaded
                .global_frequency
                .contains_key(&provider_scoped_key(APPS_PROVIDER_ID, "app:a")),
            "only the over-long entry is dropped, not the store around it"
        );
    }

    // The other side of that bound: a key of exactly `MAX_PERSISTED_KEY_LEN`
    // bytes is one a genuine [`Learning::record`] call can produce — a
    // provider sitting on [`MAX_PROVIDER_ID`] presenting an `app:`-shaped id
    // sitting on [`MAX_ITEM_ID`] — so it must survive.
    //
    // Built already provider-scoped, not as a legacy `app:` key: since this
    // bound now exceeds `MAX_ITEM_ID`, a *legacy* (unscoped) key this long
    // could never have come from a real `ItemId` in the first place — the
    // realistic maximal survivor at this exact bound is this module's own
    // output, round-tripping through a save and a load, which is exactly
    // what [`is_already_provider_scoped`] is for.
    #[test]
    fn a_persisted_key_exactly_on_the_bound_survives_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let provider = "p".repeat(MAX_PROVIDER_ID);
        let id_part = format!("app:{}", "a".repeat(MAX_ITEM_ID - 4));
        assert_eq!(id_part.len(), MAX_ITEM_ID);
        let key = provider_scoped_key(&provider, &id_part);
        assert_eq!(key.len(), MAX_PERSISTED_KEY_LEN);
        let now = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{"{key}":{{"count":9,"last_ms":{now}}}}}}}"#
            ),
        )
        .unwrap();

        assert!(
            Learning::load(&path).global_frequency.contains_key(&key),
            "a key exactly on the bound is legitimate and must still load"
        );
    }

    // The byte ceiling cannot reach this case: a FIFO has no length to
    // compare against, and reading one blocks until a writer appears —
    // forever, on a store nobody is writing to. Only refusing it before the
    // open avoids that, which is why the stat check is a separate guard
    // rather than a consequence of the ceiling.
    //
    // This test can only hang if that check is missing or is ordered after
    // the open, which is the regression it exists to catch. `mkfifo(1)` is
    // coreutils, so it needs no dependency; the test is unix-gated because a
    // FIFO is a unix concept, not because the guard is.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_rather_than_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo should have created the FIFO");

        let loaded = Learning::load(&path);

        assert!(
            loaded.is_empty(),
            "a path that is not a regular file must be refused"
        );
    }

    // The stat check refuses what a path *resolves* to, never the fact that
    // it was reached through a symlink: `~/.local/state/hop` being a symlink
    // into a different volume is an ordinary setup, and a store there must
    // still load. A symlink to `/dev/zero` resolves to a character device
    // and is refused by the same check that refuses the FIFO above.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_regular_store_is_still_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real-store.json");
        let link = dir.path().join("learning.json");

        let mut store = Learning::empty();
        store.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        store.save(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            Learning::load(&link).frequency_boost(APPS_PROVIDER_ID, "app:a") > 0,
            "a symlink to a regular file resolves to one, and must load"
        );
    }

    // `MAX_GLOBAL_ENTRIES` was enforced only in `record`, so a store that
    // arrived with more entries than that kept every one of them until the
    // next launch was recorded — and for good, in a session that records
    // none, which is any session that only reads boosts and saves. The
    // file's own count is not evidence of anything.
    #[test]
    fn a_store_over_the_entry_cap_is_evicted_down_to_it_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, store_of_entries(MAX_GLOBAL_ENTRIES + 500, now_ms())).unwrap();

        let loaded = Learning::load(&path);

        assert_eq!(
            loaded.global_frequency.len(),
            MAX_GLOBAL_ENTRIES,
            "a store over the entry cap must be evicted down to it"
        );
    }

    // Every entry in the *file* here is stamped past any conceivable clock
    // reading; the load clamps each one to the load instant, which is just
    // as recent, so `purge_expired` — which drops entries by age — keeps all
    // of them either way. The entry cap is the only thing left that can
    // bring the count down, which is what makes this the case the age filter
    // alone never covered.
    //
    // What this does *not* claim is that the surviving entries are the
    // honest ones. Eviction is by `last_ms`, and the clamp bounds a forged
    // stamp at the load instant rather than beating it: every entry that was
    // already on disk was stamped earlier than that, so future-dating still
    // survives eviction against them. See `Learning::load`'s doc comment for
    // what the clamp does and does not change here.
    #[test]
    fn future_dated_entries_do_not_evade_the_entry_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, store_of_entries(MAX_GLOBAL_ENTRIES + 500, u64::MAX)).unwrap();

        let loaded = Learning::load(&path);

        assert_eq!(
            loaded.global_frequency.len(),
            MAX_GLOBAL_ENTRIES,
            "an entry future-dated in the file is clamped to the load instant, which \
             survives the age filter just as well, and must still meet the cap"
        );
    }

    // The requirement stated in `MAX_STORE_BYTES`'s doc comment, exercised
    // rather than asserted in prose: the largest store `save` can ever write
    // must reload intact, or the ceiling would refuse this module's own
    // output. Largest means every dimension at once — `MAX_GLOBAL_ENTRIES`
    // entries, each keyed by a `MAX_PERSISTED_KEY_LEN`-byte provider-scoped
    // key (issue #72: a `MAX_PROVIDER_ID`-byte provider and a
    // `MAX_ITEM_ID`-byte id-part) made entirely of the C0 control characters
    // `serde_json` spends a six-character `\u00XX` escape on (the worst
    // expansion JSON escaping has), each carrying both numeric fields at
    // their full width.
    //
    // An id, or a provider, may hold control characters: `ItemId::new`
    // bounds an id's length and applies no content rule, a provider is a
    // bare `&str` with no validating type at all, and `hop-protocol`'s
    // content rules cover the two command-shaped outcomes, not either of
    // these.
    #[test]
    fn the_largest_store_save_can_write_still_reloads_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        // The C0 controls `serde_json` spends the full six characters on.
        // The five it has a two-character escape for (`\b`, `\t`, `\n`,
        // `\f`, `\r`) are left out, so every byte of every key below really
        // does cost six and the store really is the largest one `save` can
        // write, rather than one a few thousand bytes short of it.
        const SIX_CHARACTER_ESCAPES: [char; 16] = [
            '\u{1}', '\u{2}', '\u{3}', '\u{4}', '\u{5}', '\u{6}', '\u{7}', '\u{b}', '\u{e}',
            '\u{f}', '\u{10}', '\u{11}', '\u{12}', '\u{13}', '\u{14}', '\u{15}',
        ];

        // One provider, held fixed and maximal, is enough: what makes every
        // key distinct is the id-part below, and the provider's own bytes
        // cost the same maximal escape either way.
        let max_provider = SIX_CHARACTER_ESCAPES[0].to_string().repeat(MAX_PROVIDER_ID);

        let mut store = Learning::empty();
        for i in 0..MAX_GLOBAL_ENTRIES {
            let mut id_part = SIX_CHARACTER_ESCAPES[0].to_string().repeat(MAX_ITEM_ID - 3);
            // Three base-16 digits over that alphabet, so every key is
            // distinct and none of them is cheaper to escape than the rest.
            for digit in [i / 256, (i / 16) % 16, i % 16] {
                id_part.push(SIX_CHARACTER_ESCAPES[digit]);
            }
            assert_eq!(id_part.len(), MAX_ITEM_ID);
            let key = provider_scoped_key(&max_provider, &id_part);
            assert_eq!(key.len(), MAX_PERSISTED_KEY_LEN);
            store.global_frequency.insert(
                key,
                LearningEntry {
                    count: u32::MAX,
                    // The widest a `last_ms` can be written, and a value
                    // `save` will really persist: retention keeps anything
                    // at or past the cutoff, and a future stamp is past it.
                    last_ms: u64::MAX,
                },
            );
        }
        assert_eq!(store.global_frequency.len(), MAX_GLOBAL_ENTRIES);

        store.save(&path).unwrap();
        let written = std::fs::metadata(&path).unwrap().len();
        assert!(
            written <= MAX_STORE_BYTES,
            "save wrote {written} bytes, over the {MAX_STORE_BYTES}-byte ceiling its own \
             loads enforce"
        );

        assert_eq!(
            Learning::load(&path).global_frequency.len(),
            MAX_GLOBAL_ENTRIES,
            "a full store must survive a round trip through save and load"
        );
    }

    // A directory with no read permission cannot be opened for the fsync,
    // which must fail the whole save rather than being ignored. This drives
    // the real `save` path, not a re-implementation of it, so it also
    // covers the second acceptance criterion directly.
    //
    // Root (and some sandboxes) bypass unix permission checks entirely, in
    // which case stripping read permission below would not actually block
    // anything. Probe for that empirically with a throwaway file rather
    // than assuming — see the first assertion below for why an unenforced
    // probe fails the test outright instead of skipping it.
    #[cfg(unix)]
    #[test]
    fn save_surfaces_an_error_when_the_directory_sync_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        std::fs::write(&probe, b"x").unwrap();
        std::fs::set_permissions(&probe, fs::Permissions::from_mode(0o300)).unwrap();
        assert!(
            std::fs::File::open(&probe).is_err(),
            "unix permission checks are not enforced in this environment (running as \
             root, or a sandbox that bypasses them) — this crate only supports Linux \
             under a non-root CI user, so this probe failing is a signal about the \
             environment, not a condition this test can quietly tolerate"
        );

        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        // One successful save first, so the directory already holds the
        // destination file and the next save's rename is an overwrite
        // rather than a fresh create — neither needs anything the
        // directory's write+execute bits (kept below) don't already give.
        l.save(&path).unwrap();

        std::fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o300)).unwrap();
        let result = l.save(&path);
        // Restore before any assertion can fail out of this test and skip
        // cleanup of the tempdir.
        std::fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.expect_err("a directory that cannot be opened must fail the save");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    // --- The version and the timestamps a store carries (issue #38). ---
    //
    // Two different things a store asserts about itself, neither of which was
    // checked: which format its bytes are in, and when each entry was last
    // launched. See the module docs for what checking them does and does not
    // achieve.

    /// A store on `version`, carrying one entry stamped `last_ms`.
    fn store_at_version(version: u32, last_ms: u64) -> String {
        format!(
            r#"{{"version":{version},"global_frequency":{{"app:a":{{"count":3,"last_ms":{last_ms}}}}}}}"#
        )
    }

    #[test]
    fn a_store_on_an_unrecognized_version_is_refused_rather_than_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, store_at_version(STORE_VERSION + 1, now_ms())).unwrap();

        let loaded = Learning::load(&path);

        assert!(
            loaded.is_empty(),
            "a store this code has never written must not be read under this code's semantics"
        );
    }

    // The other side of the check: a store on the version this code writes
    // loads as it always did. Without this, refusing every store would
    // satisfy the test above.
    #[test]
    fn a_store_on_the_version_this_code_understands_is_still_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, store_at_version(STORE_VERSION, now_ms())).unwrap();

        let mut loaded = Learning::load(&path);
        loaded.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        assert!(
            loaded.frequency_boost(APPS_PROVIDER_ID, "app:a") > 0,
            "a store on the current version is ordinary learning and must load"
        );
    }

    // `Learning` derives `Default`, which zeroes `version` — so a store built
    // in memory rather than by `load` carries a version no `load` would
    // accept. `save` writing `STORE_VERSION` rather than `self.version` is
    // what keeps the version check from silently discarding the learning of
    // any caller that started from `Learning::default()` (`Pipeline`'s own
    // field, among them) instead of from a file.
    #[test]
    fn a_store_saved_from_a_default_learning_reloads_rather_than_being_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut store = Learning::default();
        store.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        store.save(&path).unwrap();

        assert!(
            Learning::load(&path).frequency_boost(APPS_PROVIDER_ID, "app:a") > 0,
            "save writes the format it actually produces, so its own output must reload"
        );
    }

    #[test]
    fn a_far_future_timestamp_is_clamped_to_the_load_instant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, store_at_version(STORE_VERSION, u64::MAX)).unwrap();

        let before = now_ms();
        let loaded = Learning::load(&path);
        let after = now_ms();

        let stamped = loaded
            .global_frequency
            .get(&provider_scoped_key(APPS_PROVIDER_ID, "app:a"))
            .unwrap()
            .last_ms;
        assert!(
            stamped >= before && stamped <= after,
            "a stamp no clock will reach must come back as the load instant, got {stamped}"
        );
    }

    // What the clamp above buys, stated as the thing the criterion asks for:
    // the entry decays. The second assertion is the behavior before this
    // change — the same clock reading, against the stamp as the file wrote
    // it, returns the raw value — so the two lines together are the
    // before-and-after, not just an assertion that division works.
    //
    // `frequency_boost` reads the clock itself, so it cannot be asked what it
    // will return in 91 days; `apply_decay` takes `now` as an argument and is
    // the same function `frequency_boost` calls.
    #[test]
    fn a_clamped_timestamp_decays_where_the_future_one_it_replaced_never_would() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, store_at_version(STORE_VERSION, u64::MAX)).unwrap();

        let loaded = Learning::load(&path);
        let clamped = loaded
            .global_frequency
            .get(&provider_scoped_key(APPS_PROVIDER_ID, "app:a"))
            .unwrap()
            .last_ms;

        let raw = 40;
        let long_after = clamped + DECAY_QUARTER_MS + 1;
        assert_eq!(
            apply_decay(raw, clamped, long_after),
            raw / 4,
            "a clamped stamp ages like any other and its boost decays"
        );
        assert_eq!(
            apply_decay(raw, u64::MAX, long_after),
            raw,
            "the same clock reading against the file's own stamp is undecayed, which is \
             what the clamp exists to prevent"
        );
    }

    // The other side of the clamp, and the reason it is a clamp rather than
    // a rewrite: a stamp in the past is what every honest entry carries, and
    // must survive a load byte for byte. A load that stamped every entry
    // `now` would pass the two tests above and destroy all decay.
    #[test]
    fn a_timestamp_in_the_past_is_left_exactly_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let a_minute_ago = now_ms() - 60_000;
        std::fs::write(&path, store_at_version(STORE_VERSION, a_minute_ago)).unwrap();

        let loaded = Learning::load(&path);

        assert_eq!(
            loaded
                .global_frequency
                .get(&provider_scoped_key(APPS_PROVIDER_ID, "app:a"))
                .unwrap()
                .last_ms,
            a_minute_ago,
            "an honest stamp is not in the future and must not be touched"
        );
    }

    // --- Reporting why a load fell back to empty (issue #43). ---
    //
    // Each test for a single condition asserts a whole `LoadReport` by
    // equality rather than "not `Loaded`". The defect this issue is about is
    // *conditions collapsing together*, so a test that only distinguished
    // failure from success would pass against the very code the issue was
    // filed on. The last test is the exception and says why: it is about
    // `load` still degrading, which is the same for every condition.
    //
    // Each also names the outcome it would most plausibly be confused with,
    // because those pairings are the whole point: absent against unreadable,
    // malformed against an unrecognized version, and the byte ceiling against
    // both.

    /// A store `save` itself would write, on disk at `path`.
    fn write_ordinary_store(path: &Path) {
        let mut store = Learning::empty();
        store.record_launch(APPS_PROVIDER_ID, "q", &ItemId::new("app:a").unwrap());
        store.save(path).unwrap();
    }

    #[test]
    fn a_store_that_loads_reports_that_it_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        write_ordinary_store(&path);

        let (loaded, report) = Learning::load_reporting(&path);

        assert_eq!(report, LoadReport::Loaded);
        assert!(
            loaded.frequency_boost(APPS_PROVIDER_ID, "app:a") > 0,
            "the reporting entry point returns the same store `load` would"
        );
    }

    #[test]
    fn an_absent_store_reports_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-written.json");

        let (loaded, report) = Learning::load_reporting(&path);

        assert_eq!(report, LoadReport::Absent);
        assert!(loaded.is_empty(), "a first run still degrades to empty");
    }

    // The pairing that matters most. Both of these are one `fs::metadata`
    // call, and reporting a permission denial as `Absent` would tell a user
    // whose state directory is broken that this is their first run, every
    // session, for ever.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_store_reports_unreadable_rather_than_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        write_ordinary_store(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

        // Mode bits do not stop root, and some filesystems ignore them
        // outright. Probing the open is what separates such a run from a real
        // denial: where the open succeeds there is no denial to report, and
        // asserting one would be asserting something this run cannot show.
        if fs::File::open(&path).is_ok() {
            eprintln!(
                "skipped: 0o000 did not deny this process a read (running as root, or a \
                 filesystem that ignores mode bits)"
            );
            return;
        }

        let (loaded, report) = Learning::load_reporting(&path);

        assert_eq!(
            report,
            LoadReport::Unreadable(io::ErrorKind::PermissionDenied),
            "a store that exists and cannot be read is not an absent one"
        );
        assert!(
            loaded.is_empty(),
            "reporting the denial does not stop the degradation"
        );
    }

    // The other route into `Unreadable`, and the one that needs no unix gate
    // and no permissions: bytes that are not UTF-8 fail in the decode rather
    // than in the open. It is `Unreadable` and not `Malformed` because nothing
    // was ever decoded to parse, so nothing here has an opinion on whether the
    // bytes were a store — and it is not `TooLarge` either, which the test
    // pairing this one under the ceiling covers from the other side.
    #[test]
    fn a_store_that_is_not_utf8_reports_unreadable_rather_than_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();

        assert_eq!(
            Learning::load_reporting(&path).1,
            LoadReport::Unreadable(io::ErrorKind::InvalidData)
        );
    }

    #[test]
    fn a_malformed_store_is_reported_rather_than_silently_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, "{not json").unwrap();

        let (loaded, report) = Learning::load_reporting(&path);

        assert_eq!(report, LoadReport::Malformed);
        assert!(
            loaded.is_empty(),
            "a malformed store still degrades to empty"
        );
    }

    // Valid JSON of the wrong shape is malformed in exactly the same sense:
    // these bytes are not a store, whether they parse as JSON or not.
    #[test]
    fn valid_json_of_the_wrong_shape_reports_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, "[1,2,3]").unwrap();

        assert_eq!(Learning::load_reporting(&path).1, LoadReport::Malformed);
    }

    // The second pairing. A store on another version is *well-formed* — a v2
    // file written by a later hop parses perfectly and is refused for what it
    // says about itself, not for being damaged. Reporting it as `Malformed`
    // would tell a user who downgraded that their store is corrupt.
    #[test]
    fn an_unrecognized_version_is_reported_separately_from_a_malformed_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let newer = STORE_VERSION + 1;
        std::fs::write(&path, store_at_version(newer, now_ms())).unwrap();

        let (loaded, report) = Learning::load_reporting(&path);

        assert_eq!(report, LoadReport::UnrecognizedVersion { found: newer });
        assert!(loaded.is_empty(), "the store is still refused whole");
    }

    // The test above writes a v2 that happens to keep every v1 field, which is
    // the *least* likely v2 there is: a version is bumped precisely because
    // the shape changed. So this is the case that decides whether the variant
    // above means what it says. While the version was read only after a full
    // parse had already succeeded, a v2 that moved its table failed both
    // parses and came back `Malformed` — telling a user who downgraded that
    // their live store was damaged.
    #[test]
    fn a_later_version_that_changed_shape_reports_the_version_and_not_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let newer = STORE_VERSION + 1;
        std::fs::write(
            &path,
            format!(r#"{{"version":{newer},"entries":{{"app:a":{{"n":9}}}}}}"#),
        )
        .unwrap();

        assert_eq!(
            Learning::load_reporting(&path).1,
            LoadReport::UnrecognizedVersion { found: newer },
            "a store this code cannot parse because it is a later format is not a damaged one"
        );
    }

    // The same claim reduced to the least a document can say: it announces a
    // version and offers nothing else to parse.
    #[test]
    fn a_document_carrying_only_an_unrecognized_version_reports_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let newer = STORE_VERSION + 1;
        std::fs::write(&path, format!(r#"{{"version":{newer}}}"#)).unwrap();

        assert_eq!(
            Learning::load_reporting(&path).1,
            LoadReport::UnrecognizedVersion { found: newer }
        );
    }

    // The other side of reading the version first, and the reason the probe is
    // not simply "anything unparseable is a version problem": a document with
    // no `version` to read announces nothing, so it is not a store, whatever
    // else it holds. Without this, reporting `UnrecognizedVersion` for
    // everything that fails to parse would satisfy the two tests above.
    #[test]
    fn a_document_with_no_version_field_reports_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, r#"{"global_frequency":{}}"#).unwrap();

        assert_eq!(Learning::load_reporting(&path).1, LoadReport::Malformed);
    }

    // And the version being read first must not swallow the ordinary damage
    // case: a document on the version this code does write, whose store body
    // is not one, is `Malformed` and not a version problem.
    #[test]
    fn a_current_version_document_that_is_not_a_store_reports_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(
            &path,
            format!(r#"{{"version":{STORE_VERSION},"global_frequency":[]}}"#),
        )
        .unwrap();

        assert_eq!(Learning::load_reporting(&path).1, LoadReport::Malformed);
    }

    // A FIFO, a directory or a character device is not damaged, absent or
    // over any ceiling: the path simply does not name a store. `read_bounded_store`
    // refuses it before the open, and the report says which of its guards did.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_a_regular_file_reports_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo should have created the FIFO");

        assert_eq!(
            Learning::load_reporting(&path).1,
            LoadReport::NotARegularFile
        );
    }

    // The third pairing. The bytes over the ceiling here are a perfectly
    // well-formed store — `whitespace_padded_store` pads a real one — so
    // reporting `Malformed` would name the wrong cause, and the file is
    // plainly present, so reporting `Absent` would too.
    #[test]
    fn a_store_over_the_byte_ceiling_reports_the_ceiling_and_not_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        std::fs::write(&path, whitespace_padded_store(MAX_STORE_BYTES as usize + 1)).unwrap();

        assert_eq!(Learning::load_reporting(&path).1, LoadReport::TooLarge);
    }

    // The ceiling must not depend on where a codepoint happens to fall. Every
    // byte of the file below is valid UTF-8, but `take` cuts at a byte offset,
    // and here the cut lands between the two bytes of `é` — so the bytes that
    // came back were not valid UTF-8 even though the store was. Decoding
    // before measuring reported `Unreadable(InvalidData)` for what is purely
    // an over-size store, which is both fallbacks wearing one variant and is
    // exactly the collapse this issue exists to end.
    #[test]
    fn a_store_over_the_ceiling_reports_the_ceiling_even_when_the_cut_splits_a_character() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        // Padded to the ceiling exactly, then one two-byte character. `take`
        // stops after `MAX_STORE_BYTES + 1` bytes, which is that character's
        // first byte and not its second.
        let mut bytes = whitespace_padded_store(MAX_STORE_BYTES as usize).into_bytes();
        bytes.extend_from_slice("é".as_bytes());
        assert_eq!(bytes.len() as u64, MAX_STORE_BYTES + 2);
        assert!(
            std::str::from_utf8(&bytes).is_ok(),
            "the file itself must be valid UTF-8, or this would prove nothing"
        );
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(
            Learning::load_reporting(&path).1,
            LoadReport::TooLarge,
            "an over-size store is over-size whatever the read's cut landed on"
        );
    }

    // The acceptance criterion that the infallible entry point is unchanged,
    // held against every condition the reporting one distinguishes: whatever
    // the report would have said, `load` still hands back an empty store and
    // still cannot fail.
    #[test]
    fn the_infallible_entry_point_still_degrades_to_empty_on_every_reported_condition() {
        let dir = tempfile::tempdir().unwrap();

        let absent = dir.path().join("never-written.json");

        let malformed = dir.path().join("malformed.json");
        std::fs::write(&malformed, "{not json").unwrap();

        let wrong_version = dir.path().join("wrong-version.json");
        std::fs::write(
            &wrong_version,
            store_at_version(STORE_VERSION + 1, now_ms()),
        )
        .unwrap();

        let too_large = dir.path().join("too-large.json");
        std::fs::write(
            &too_large,
            whitespace_padded_store(MAX_STORE_BYTES as usize + 1),
        )
        .unwrap();

        for path in [&absent, &malformed, &wrong_version, &too_large] {
            assert!(
                Learning::load(path).is_empty(),
                "`load` must still degrade to empty for {}",
                path.display()
            );
            assert_ne!(
                Learning::load_reporting(path).1,
                LoadReport::Loaded,
                "and the reporting sibling must still call it a fallback for {}",
                path.display()
            );
        }
    }

    // --- The manifest is the sole authority for plaintext persistence
    // (issue #72, Decision 2's manifest half). ---
    //
    // `persistence_key` is the one function deciding what an id looks like
    // once it can reach disk; these pin its rule directly (a plain `bool` in,
    // one of two partitions out — no inspection of `raw_id` at all), then pin
    // the two things that rule is worthless without: the same key on both
    // sides of a restart, and a legacy store's plaintext migrating in place
    // rather than going unmatched forever.

    // The rule itself: `persistence_key`'s `bool` argument decides the
    // partition outright, and nothing about the raw id's own shape moves it
    // from one side to the other. Held fixed at `APPS_PROVIDER_ID` throughout
    // — this test is about the id-part decision, not about the provider fold
    // on top of it, which has its own tests below (see "A provider cannot
    // forge another provider's key").
    #[test]
    fn persistence_key_partitions_on_the_bool_alone_not_on_the_raw_ids_shape() {
        let id_part_of = |key: &str| {
            key.strip_prefix(&provider_scoped_key(APPS_PROVIDER_ID, ""))
                .expect("every key here is scoped to APPS_PROVIDER_ID")
                .to_string()
        };

        for raw in [
            "calc:2+2",
            "app:firefox.desktop",
            "some-future-provider:opaque-payload",
            "sha256:not-a-real-hash",
        ] {
            assert_eq!(
                id_part_of(&persistence_key(APPS_PROVIDER_ID, raw, true)),
                raw,
                "persist_plaintext: true must write {raw:?} through verbatim, \
                 whatever it looks like"
            );
            let hashed = id_part_of(&persistence_key(APPS_PROVIDER_ID, raw, false));
            assert_eq!(
                hashed,
                format!("sha256:{:x}", Sha256::digest(raw.as_bytes())),
                "persist_plaintext: false must hash {raw:?} under the raw id"
            );
            assert_ne!(
                hashed, raw,
                "a hashed key must never equal its raw input verbatim"
            );
        }
    }

    // The brief's own minimum: a provider whose manifest does *not* opt in
    // hashes even an id in the exact shape issue #39's shape rule used to
    // wave through unconditionally. `APPS_PROVIDER_ID` presenting `app:`
    // proves the point precisely because `app:` was the one shape the old
    // rule trusted unconditionally — under the manifest rule it is no safer
    // than any other shape the moment the manifest says so.
    #[test]
    fn a_non_opted_in_providers_id_hashes_even_in_an_old_known_safe_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);
        // Deliberately not synced: `APPS_PROVIDER_ID` is not in the
        // (empty) plaintext set, so it hashes exactly like any other
        // provider this store has never heard of.

        store.record_launch(
            APPS_PROVIDER_ID,
            "firefox",
            &ItemId::new("app:firefox").unwrap(),
        );
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains("\"app:firefox\"")
                && !saved.contains(&provider_scoped_key(APPS_PROVIDER_ID, "app:firefox")),
            "an app:-shaped id from a provider that did not opt in must not persist \
             in the clear, got: {saved}"
        );
        assert!(
            saved.contains("sha256:"),
            "it must be hashed instead, got: {saved}"
        );
    }

    // The other required minimum: an opted-in provider's id persists in the
    // clear regardless of its shape — proving the flag is the *sole*
    // authority rather than one more condition alongside a shape check. A
    // provider id and an id shape that share nothing with `app:`/`utility:`/
    // `web-search:` is the point: nothing here would have passed issue #39's
    // rule, and it persists anyway because the manifest says so.
    #[test]
    fn an_opted_in_providers_id_persists_in_the_clear_regardless_of_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);
        store.sync_plaintext_providers(["widget".to_string()]);

        store.record_launch(
            "widget",
            "banana",
            &ItemId::new("widget-payload:banana-split").unwrap(),
        );
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            saved.contains(&format!(
                "\"{}\"",
                provider_scoped_key("widget", "widget-payload:banana-split")
            )),
            "an opted-in provider's id must persist in the clear even in a shape \
             no known-safe prefix would ever have matched, got: {saved}"
        );

        let reloaded = Learning::load(&path);
        assert_eq!(
            reloaded
                .global_frequency
                .get(&provider_scoped_key(
                    "widget",
                    "widget-payload:banana-split"
                ))
                .map(|e| e.count),
            Some(1),
            "the key round-trips through a save and load unchanged — it is \
             already shaped like this module's own provider-scoped output, so \
             the load-time legacy migration leaves it alone"
        );
    }

    // The fail-closed requirement itself: a provider absent from the synced
    // set — one this process has never learned opted in, including one that
    // simply never registered at all — hashes, even while *other* providers
    // in the very same store are opted in. Presence in the set is what
    // grants plaintext persistence; nothing else does, and nothing about
    // being "the provider on this launch" substitutes for it.
    #[test]
    fn an_id_from_a_provider_absent_from_the_synced_set_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);
        // "apps" is synced in; "mystery" — the provider actually presenting
        // this launch — is not, and never was.
        store.sync_plaintext_providers(["apps".to_string()]);

        store.record_launch(
            "mystery",
            "anything",
            &ItemId::new("app:mystery-item").unwrap(),
        );
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains(&provider_scoped_key("mystery", "app:mystery-item")),
            "a provider absent from the synced set must never persist in the \
             clear merely because some other provider is opted in, got: {saved}"
        );
        assert!(
            saved.contains("sha256:"),
            "it must be hashed instead, got: {saved}"
        );
    }

    // --- Revocation: a stored key's shape is re-checked against the
    // provider's *current* manifest flag, not just left alone forever
    // because it once round-tripped through this module's own output shape.

    // The scenario the finding is about: a provider opts in, persists an id
    // in the clear, then revokes — its already-stored key must not sit on
    // disk in the clear for the rest of the retention window just because
    // nothing used to re-check it against the provider's current answer.
    #[test]
    fn a_revoked_providers_plaintext_entry_is_hashed_on_the_next_sync_and_keeps_its_count() {
        let mut store = Learning::empty();
        store.sync_plaintext_providers(["widget".to_string()]);
        for _ in 0..5 {
            store.record_launch("widget", "banana", &ItemId::new("widget:banana").unwrap());
        }
        let plaintext_key = provider_scoped_key("widget", "widget:banana");
        assert_eq!(
            store.global_frequency.get(&plaintext_key).map(|e| e.count),
            Some(5),
            "sanity check: the entry must actually be plaintext before revocation"
        );

        // Revoke: the next sync no longer includes "widget".
        store.sync_plaintext_providers([]);

        assert!(
            !store.global_frequency.contains_key(&plaintext_key),
            "the plaintext key must not survive a sync that revokes its provider"
        );
        let hashed_key = provider_scoped_key(
            "widget",
            &format!("sha256:{:x}", Sha256::digest(b"widget:banana")),
        );
        assert_eq!(
            store.global_frequency.get(&hashed_key).map(|e| e.count),
            Some(5),
            "the count must survive the re-hash, under the key a future \
             (hashed) lookup for \"widget\" will actually compute"
        );
    }

    // The other side: a provider that is *still* opted in must not have its
    // plaintext entry disturbed just because a sync happened. Without this,
    // a passing revocation test could hide a bug that re-hashes everything
    // indiscriminately rather than only what actually lost its opt-in.
    #[test]
    fn a_still_opted_in_providers_plaintext_entry_is_untouched_across_a_sync() {
        let mut store = Learning::empty();
        store.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        store.record_launch(
            APPS_PROVIDER_ID,
            "firefox",
            &ItemId::new("app:firefox").unwrap(),
        );
        let plaintext_key = provider_scoped_key(APPS_PROVIDER_ID, "app:firefox");
        assert!(store.global_frequency.contains_key(&plaintext_key));

        // Synced again with the identical set — apps never revoked.
        store.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);

        assert_eq!(
            store.global_frequency.get(&plaintext_key).map(|e| e.count),
            Some(1),
            "a still-opted-in provider's plaintext entry must be untouched \
             by a sync, not merely eventually restored to it"
        );
    }

    // The "absent from the synced set" case, pinned on its own: this store
    // never learned "apps" opted in through any sync at all — the entry
    // reaches its plaintext shape through the *load-time legacy migration*
    // (`rekeyed_legacy_key`, which re-attributes a v1 `app:` key to the apps
    // provider unconditionally, before any registry answer exists) rather
    // than through an explicit prior opt-in. Established in
    // `Learning::rehash_entries_for_providers_no_longer_opted_in`'s own doc
    // comment: there is no third state between "in the set" and "not", so
    // "absent" gets the identical treatment as an explicit revocation —
    // this is that case, not the revocation case above with different
    // words.
    #[test]
    fn an_entry_whose_provider_is_absent_from_the_synced_set_is_hashed_same_as_a_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let now = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{"app:firefox":{{"count":7,"last_ms":{now}}}}}}}"#
            ),
        )
        .unwrap();

        let mut loaded = Learning::load(&path);
        let plaintext_key = provider_scoped_key(APPS_PROVIDER_ID, "app:firefox");
        assert_eq!(
            loaded.global_frequency.get(&plaintext_key).map(|e| e.count),
            Some(7),
            "sanity check: the legacy key re-attributes to the apps provider \
             on load regardless of any sync having happened yet"
        );

        // The registry answer arrives, and "apps" is not in it.
        loaded.sync_plaintext_providers(["some-other-provider".to_string()]);

        assert!(
            !loaded.global_frequency.contains_key(&plaintext_key),
            "a provider merely absent from the synced set must not keep a \
             plaintext entry any more than an explicitly revoked one would"
        );
        let hashed_key = provider_scoped_key(
            APPS_PROVIDER_ID,
            &format!("sha256:{:x}", Sha256::digest(b"app:firefox")),
        );
        assert_eq!(
            loaded.global_frequency.get(&hashed_key).map(|e| e.count),
            Some(7),
            "the count must survive the re-hash"
        );
    }

    // Correctness guard against the fix's most likely regression: an entry
    // that is already hashed must not be hashed *again* just because its
    // provider is absent from a sync — that would silently orphan it (a
    // future lookup recomputes the single hash, never the double one) for
    // no security benefit, since it was never in the clear to begin with.
    #[test]
    fn an_already_hashed_entry_is_not_rehashed_again_when_its_provider_is_absent_from_a_sync() {
        let mut store = Learning::empty();
        // Recorded while "widget" was never opted in — `record`'s own
        // persist_plaintext: false path produces this shape directly.
        store.record_launch("widget", "banana", &ItemId::new("widget:banana").unwrap());
        let hashed_key = provider_scoped_key(
            "widget",
            &format!("sha256:{:x}", Sha256::digest(b"widget:banana")),
        );
        assert!(store.global_frequency.contains_key(&hashed_key));

        store.sync_plaintext_providers([]);

        assert_eq!(
            store.global_frequency.get(&hashed_key).map(|e| e.count),
            Some(1),
            "an entry that is already hashed must not be hashed again"
        );
    }

    // The persisted bytes, not the in-memory map: a `calc:` id carries the
    // raw expression the user typed, and this is the leak issue #39 exists
    // to close (`crates/hopd/src/calculator.rs` mints these ids from routed
    // query text).
    #[test]
    fn a_calc_id_with_an_embedded_expression_never_appears_verbatim_in_the_persisted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        store.record_launch(APPS_PROVIDER_ID, "2+2", &ItemId::new("calc:2+2").unwrap());
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains("calc:2+2"),
            "a calc: id's raw expression must not be persisted verbatim, got: {saved}"
        );
        assert!(
            saved.contains("sha256:"),
            "the hashed form should be what was written instead, got: {saved}"
        );
    }

    // Issue #39's acceptance criterion, in its own literal words: "An id
    // with an embedded path never appears verbatim on disk." Every other
    // test in this section exercises that through a `calc:`/`utility:`/
    // `sha256:`-shaped id; none used a path-shaped one, which is the exact
    // scenario the criterion names — a file-provider id is not a shape this
    // module's structural argument treats specially, but the brief's own
    // example is worth pinning directly rather than trusting the general
    // case to cover it.
    #[test]
    fn a_file_path_id_never_appears_verbatim_in_the_persisted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let raw = "file:/home/user/Documents/medical-results.pdf";
        let mut store = Learning::load(&path);

        store.record_launch(APPS_PROVIDER_ID, "medical", &ItemId::new(raw).unwrap());
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains("medical-results")
                && !saved.contains("/home/user")
                && !saved.contains(raw),
            "a file: id's embedded path must not be persisted verbatim, got: {saved}"
        );
        let expected_key = format!("sha256:{:x}", Sha256::digest(raw.as_bytes()));
        assert!(
            saved.contains(&expected_key),
            "the stored key should be the hash of the raw path, got: {saved}"
        );
    }

    // The restart-survival constraint from the brief, and the one that fails
    // if the key is ever applied only at save time: `record` and
    // `frequency_boost` have to compute the *same* key from the *same* raw
    // id, or a reload keys the map by hash while a lookup still keys by the
    // id it was given, and a hashed provider's learning silently stops
    // applying the moment hopd restarts.
    #[test]
    fn a_hashed_ids_learning_survives_a_save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let item_id = ItemId::new("calc:2+2").unwrap();

        let mut store = Learning::load(&path);
        store.record_launch(APPS_PROVIDER_ID, "2+2", &item_id);
        store.save(&path).unwrap();

        let reloaded = Learning::load(&path);
        assert!(
            reloaded.boost_for(APPS_PROVIDER_ID, "2+2", &item_id) > 0.0,
            "the same raw id must still receive its boost after a restart"
        );
    }

    // An opted-in provider is the one case where the persisted bytes should
    // name the id in the clear — the opposite assertion from the two tests
    // above, and both are needed to pin the partition on the disk-writing
    // side rather than only inside `persistence_key`.
    #[test]
    fn an_app_id_round_trips_as_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let item_id = ItemId::new("app:firefox").unwrap();

        let mut store = Learning::load(&path);
        store.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        store.record_launch(APPS_PROVIDER_ID, "firefox", &item_id);
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        // "In the clear" means the id-part is legible, not that the raw id
        // is the whole key — issue #72 wraps it in the provider-scoped
        // composition, so the persisted key is `provider_scoped_key`'s
        // output over the two, not the bare quoted id.
        assert!(
            saved.contains(&format!(
                "\"{}\"",
                provider_scoped_key(APPS_PROVIDER_ID, "app:firefox")
            )),
            "an opted-in provider's app: id should persist with its id-part in the clear, \
             got: {saved}"
        );

        let mut reloaded = Learning::load(&path);
        reloaded.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        assert!(reloaded.boost_for(APPS_PROVIDER_ID, "firefox", &item_id) > 0.0);
    }

    // The partition's own edge case: an id that already begins `sha256:`,
    // from a provider not synced as plaintext-eligible (this store is never
    // synced), is hashed like any other id rather than being written through
    // as though it already were a persistence key — see `persistence_key`'s
    // doc comment for why that matters for the partition being provable at
    // all. This is the short, non-hash-shaped case; the full 64-hex-character
    // case below is the exposing shape the brief specifically asks for.
    #[test]
    fn an_id_beginning_sha256_is_hashed_rather_than_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let raw = "sha256:not-a-real-hash";
        let item_id = ItemId::new(raw).unwrap();

        let mut store = Learning::load(&path);
        store.record_launch(APPS_PROVIDER_ID, "q", &item_id);
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains(raw),
            "an id that already looks hashed must not be written through verbatim, got: {saved}"
        );
        let expected = format!("sha256:{:x}", Sha256::digest(raw.as_bytes()));
        assert!(
            saved.contains(&expected),
            "it should be hashed again, under sha256(raw_id) and not raw_id itself, got: {saved}"
        );
    }

    /// A raw id shaped *exactly* like this module's own persistence-hash
    /// output: `sha256:` followed by 64 lowercase hex characters, not a
    /// short look-alike. Nothing on the record path treats an id already
    /// shaped this way as pre-computed — [`persistence_key`] hashes it like
    /// any other unrecognized raw id — so this must still be hashed rather
    /// than written through.
    fn hash_shaped_raw_id() -> String {
        format!("sha256:{}", "deadbeef".repeat(8))
    }

    // The brief's own required pin, with the exact exposing shape: a raw id
    // that is a syntactically valid 64-character hex digest must still be
    // hashed on the record path, and must not appear verbatim in the
    // written file's bytes.
    #[test]
    fn a_raw_id_shaped_exactly_like_a_persistence_hash_is_hashed_rather_than_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let raw = hash_shaped_raw_id();
        assert_eq!(
            raw.len(),
            "sha256:".len() + 64,
            "must be the exact hash shape, not a short look-alike"
        );
        let item_id = ItemId::new(raw.clone()).unwrap();

        let mut store = Learning::load(&path);
        store.record_launch(APPS_PROVIDER_ID, "q", &item_id);
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains(&raw),
            "a raw id already shaped like a persistence hash must still be hashed on the \
             record path rather than written through verbatim, got: {saved}"
        );
        let expected = format!("sha256:{:x}", Sha256::digest(raw.as_bytes()));
        assert!(
            saved.contains(&expected),
            "it should be hashed under sha256(raw_id), not passed through as raw_id \
             itself, got: {saved}"
        );
    }

    // The end-to-end consistency this fix depends on: record stores this
    // shape under sha256(raw_id) (the test above), load's re-keying pass
    // recognizes the stored key as already a persistence hash and leaves it
    // alone (`rekeyed_global_frequency`), and `frequency_boost`'s lookup
    // recomputes sha256(raw_id) fresh from the same raw id — so the boost
    // must still apply after a restart, exactly as for any other hashed id.
    // A regression that put the idempotency guard back inside
    // `persistence_key` itself would still pass this test (both record and
    // lookup would agree on leaving the id alone) but would fail the test
    // above; a regression that dropped the guard entirely would fail this
    // one instead, by double-hashing on load. Both tests are needed.
    #[test]
    fn a_raw_id_shaped_like_a_persistence_hash_still_receives_its_boost_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let raw = hash_shaped_raw_id();
        let item_id = ItemId::new(raw).unwrap();

        let mut store = Learning::load(&path);
        store.record_launch(APPS_PROVIDER_ID, "q", &item_id);
        store.save(&path).unwrap();

        let reloaded = Learning::load(&path);
        assert!(
            reloaded.boost_for(APPS_PROVIDER_ID, "q", &item_id) > 0.0,
            "a raw id shaped like a persistence hash must still receive its boost after \
             a restart"
        );
    }

    // Issue #72, option A: a legacy plaintext `app:` key — the one shape
    // this code can attribute to an honest owner — is re-attributed to
    // `APPS_PROVIDER_ID` on load, keeping its count, and a re-save persists
    // it under the new provider-scoped key rather than the bare legacy one.
    #[test]
    fn a_legacy_plaintext_app_key_is_reattributed_to_the_apps_provider_and_keeps_its_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let now = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{"app:firefox":{{"count":7,"last_ms":{now}}}}}}}"#
            ),
        )
        .unwrap();

        let mut loaded = Learning::load(&path);
        let expected_key = provider_scoped_key(APPS_PROVIDER_ID, "app:firefox");
        assert_eq!(
            loaded.global_frequency.get(&expected_key).map(|e| e.count),
            Some(7),
            "the count must survive re-attribution on load"
        );
        // The read side agrees: a caller that looks this id up as the apps
        // provider's finds it, exactly as it would for a fresh recording —
        // once synced with the manifest's own answer that apps opts in
        // (`rekeyed_legacy_key`'s re-attribution runs regardless of syncing,
        // but a lookup that disagreed about the plaintext/hash partition
        // would still miss the entry it just re-attributed).
        loaded.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        assert!(loaded.frequency_boost(APPS_PROVIDER_ID, "app:firefox") > 0);

        loaded.save(&path).unwrap();
        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains("\"app:firefox\""),
            "the legacy unscoped key must not be re-persisted verbatim, got: {saved}"
        );
        assert!(
            saved.contains(&expected_key),
            "the re-attributed, provider-scoped key should be what was written instead, \
             got: {saved}"
        );
    }

    // Every other legacy shape — a `calc:` id, and (per the same reasoning,
    // see `rekeyed_legacy_key`'s doc comment) a legacy `utility:`/
    // `web-search:`/`sha256:` one too — has no honest owner this code can
    // invent, so option A drops it outright rather than re-hashing or
    // re-attributing it. Dropped, not merely unreachable: the entry is gone
    // from the loaded map entirely, and a re-save does not bring it back.
    #[test]
    fn every_other_legacy_shape_is_dropped_on_load_rather_than_rekeyed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let now = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{
                    "calc:2+2":{{"count":7,"last_ms":{now}}},
                    "utility:calculator:2+2":{{"count":3,"last_ms":{now}}},
                    "web-search:duckduckgo:https%3A%2F%2Fexample":{{"count":2,"last_ms":{now}}},
                    "sha256:{}":{{"count":1,"last_ms":{now}}},
                    "app:survivor":{{"count":9,"last_ms":{now}}}
                }}}}"#,
                "deadbeef".repeat(8)
            ),
        )
        .unwrap();

        let loaded = Learning::load(&path);
        assert_eq!(
            loaded.global_frequency.len(),
            1,
            "every legacy shape but app: must be dropped, leaving only the survivor, got {:?}",
            loaded.global_frequency
        );
        assert!(
            loaded
                .global_frequency
                .contains_key(&provider_scoped_key(APPS_PROVIDER_ID, "app:survivor")),
            "the one app: entry must still survive, re-attributed to the apps provider"
        );

        loaded.save(&path).unwrap();
        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        for gone in ["calc:2+2", "utility:calculator", "web-search:duckduckgo"] {
            assert!(
                !saved.contains(gone),
                "a dropped legacy entry must not reappear on a re-save, got: {saved}"
            );
        }
    }

    // Review coverage gap, ported from before issue #72: a legacy entry
    // re-attributed on load, and then the *same* raw id launched again in
    // the new session under the provider it was attributed to, must land on
    // the one entry `record`'s own insert already re-keyed it to — not a
    // second, parallel entry that only merges on some later load. Both paths
    // (load's re-attribution and `record`'s `persistence_key` insert) are
    // guaranteed to compute the same key by construction for an `app:` id
    // under `APPS_PROVIDER_ID`, but nothing before this exercised a load
    // immediately followed by new activity on the same id in one running
    // process.
    #[test]
    fn a_legacy_entry_reattributed_on_load_and_relaunched_in_the_same_session_is_one_entry_not_two()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let now = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{STORE_VERSION},"global_frequency":{{"app:firefox":{{"count":7,"last_ms":{now}}}}}}}"#
            ),
        )
        .unwrap();

        let mut loaded = Learning::load(&path);
        // Without this, the fresh launch below would hash "app:firefox"
        // (unsynced defaults to `false`) and land on a *different* key from
        // the one the legacy migration re-attributed, defeating the merge
        // this test exists to pin.
        loaded.sync_plaintext_providers([APPS_PROVIDER_ID.to_string()]);
        loaded.record_launch(
            APPS_PROVIDER_ID,
            "firefox",
            &ItemId::new("app:firefox").unwrap(),
        );

        assert_eq!(
            loaded.global_frequency.len(),
            1,
            "the re-attributed legacy entry and the freshly recorded launch of the same \
             raw id under the same provider must be one entry, not two"
        );
        let key = provider_scoped_key(APPS_PROVIDER_ID, "app:firefox");
        assert_eq!(
            loaded.global_frequency.get(&key).map(|e| e.count),
            Some(8),
            "the count must reflect both the migrated legacy launch and the new one"
        );
    }

    // --- No provider can collect another provider's boost (issue #72). ---
    //
    // The issue's own scenario, pinned on both halves of `boost_for`
    // separately (`frequency_boost`'s persisted path here,
    // `query_boost`'s in-memory path below) and once more end to end: a
    // provider that presents another provider's item id must never receive
    // the boost the genuine provider earned on it, and the genuine provider
    // must go on receiving it undisturbed.

    /// The issue's own scenario, on `boost_for` — the sum of both halves,
    /// and the value the ranker actually consumes.
    #[test]
    fn evil_presenting_apps_item_id_gets_no_boost_from_apps_launches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        let firefox = ItemId::new("app:firefox").unwrap();
        for _ in 0..10 {
            store.record_launch(APPS_PROVIDER_ID, "firefox", &firefox);
        }

        assert_eq!(
            store.boost_for("evil", "firefox", &firefox),
            0.0,
            "a provider presenting another provider's item id must not inherit its boost"
        );
        assert!(
            store.boost_for(APPS_PROVIDER_ID, "firefox", &firefox) > 0.0,
            "the genuine provider must still receive the boost it earned"
        );
    }

    /// The same scenario, isolated to `query_boost`'s in-memory `selections`
    /// path — the half that is never persisted, and so is easy to leave
    /// unscoped if only `frequency_boost` were fixed. `record_launch` seeds
    /// both tables at once; this asserts on `query_boost` directly rather
    /// than through `boost_for`'s sum, so a regression that scoped only
    /// `global_frequency` would still fail here.
    #[test]
    fn evil_presenting_apps_item_id_gets_no_query_boost_from_apps_selections() {
        let mut l = Learning::empty();
        l.record_launch(
            APPS_PROVIDER_ID,
            "firefox",
            &ItemId::new("app:firefox").unwrap(),
        );

        assert_eq!(
            l.query_boost("evil", "firefox", "app:firefox"),
            0,
            "the in-memory selections table must not answer for a different provider's id"
        );
        assert!(
            l.query_boost(APPS_PROVIDER_ID, "firefox", "app:firefox") > 0,
            "the genuine provider's query boost must still apply"
        );
    }

    /// The forgery the composition has to resist, made concrete: a naive
    /// `format!("{provider}:{id}")` join makes `("apps", "app:firefox")` and
    /// `("apps:app", "firefox")` the identical string — the provider and the
    /// id disagree about where the boundary is, and a bare join has no way
    /// to arbitrate. [`provider_scoped_key`]'s length prefix is what
    /// resolves that; this pins it two ways: directly on the composition
    /// function, and end to end through a genuine recorded launch that the
    /// forged pairing must not be able to reach.
    #[test]
    fn a_provider_id_containing_the_composition_separator_cannot_forge_another_providers_key() {
        let honest = provider_scoped_key("apps", "app:firefox");
        let forged = provider_scoped_key("apps:app", "firefox");
        assert_ne!(
            honest, forged,
            "a provider id containing the separator must not land on another \
             provider's key by splitting it differently"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);
        let genuine_id = ItemId::new("app:firefox").unwrap();
        for _ in 0..10 {
            store.record_launch("apps", "firefox", &genuine_id);
        }

        let forged_id = ItemId::new("firefox").unwrap();
        assert_eq!(
            store.boost_for("apps:app", "firefox", &forged_id),
            0.0,
            "splitting the separator across the provider and the id must not forge \
             the genuine key"
        );
    }

    /// The decimal digit count [`MAX_PERSISTED_KEY_LEN`] is derived from —
    /// pinned directly so a future change to [`MAX_PROVIDER_ID`] that
    /// crosses a power of ten (a bound of 100 or more, say) fails here
    /// rather than silently under-counting that constant. See
    /// [`MAX_PROVIDER_ID_DIGITS`]'s own doc comment.
    #[test]
    fn max_provider_id_decimal_digits_matches_its_own_digit_count() {
        assert_eq!(
            MAX_PROVIDER_ID_DIGITS,
            MAX_PROVIDER_ID.to_string().len(),
            "MAX_PROVIDER_ID_DIGITS must track MAX_PROVIDER_ID's actual decimal digit count"
        );
    }
}
