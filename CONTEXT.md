# CONTEXT

The vocabulary this codebase uses. When naming something — a type, a test, an
issue title, a comment — use the term as defined here rather than a synonym.

Seeded at the end of M1, from the terms the core crates actually settled on.
It describes what exists; extend it as later milestones resolve new terms.

## Items and actions

**Item** — one result. Never "result", "row", "hit" or "entry" in code or
prose. An item carries its id, kind, title, optional subtitle and icon, its
available actions, which of them is default, optional copy text, the id of the
provider that produced it, and its `append_to_end` flag.

**Kind** — what an item *is*: `App`, `Window`, `File`, `Calculator`,
`Currency`, `Timezone`, `Weather`, `Emoji`, `WebSearch`, `Action`. A closed
set, defined in `hop-protocol`.

Two kinds from the old GNOME extension are deliberately gone. `recent` has no
equivalent — there is no recents concept in the protocol. `utility` split into
the four kinds `Calculator`, `Currency`, `Timezone` and `Weather`. Ported tests
that referenced either carry a `// DIVERGENCE:` comment saying so.

**Action** — something you can do to an item: open, focus, copy, run, close a
window, move to a workspace, open a URL. An item's **default action** is the
one Enter runs.

**Provider** — a source of items. Describes itself with a **manifest** (its id,
the kinds it produces, the modes it serves, a minimum term length, a per-query
budget) and answers queries. This is the plugin seam; every later extension
tier adapts to it.

## Queries

Three different strings travel together, and confusing them causes real bugs:

**Raw query** — exactly what the user typed, untouched, whitespace and all.
Carried as `RoutedQuery::raw`.

**Term** — the raw query with any explicit prefix stripped and trimmed, plus,
where routing matched a known key rather than just a shape, the canonical form
of that key: an alias-matched timezone query carries the alias key it matched
(`sao_paulo`), not the spelling that was typed. This is what search means by
"the query". Carried as `RoutedQuery::term`.

**Effective term** — the term after an alias rewrite, if one applied. This is
what ranking scores against. It is *not* what learning is keyed on: learning
keys on the term, before any rewrite — an alias rewrite is a ranking
substitution the user never typed. The term is not always the typed spelling
either, though: where routing canonicalized it, `sao paulo` and `SAO PAULO`
learn as one key rather than two.

**Mode** — how a query should be interpreted. One of `All`, `Windows`, `Apps`,
`Files`, `Emoji`, `Timezone`, `Currency`, `Calculator`, `Weather`, `Actions`,
`WebSearch`. Distinct from `Kind`: a mode is an interpretation of a query, a
kind is a property of an item. Every mode except `All` maps to exactly one
kind today, which is a fact about the current mapping and not a guarantee.

**Routing** — deciding a raw query's mode. Pure, and runs on every keystroke.

## Exclusive, inferred, and augment-not-hijack

**Exclusive** — the query named its mode with an explicit prefix (`w `, `$`,
`=`, a trailing ` weather`…). Results are filtered to that mode's kinds and
nothing else shows.

**Inferred** — the mode was deduced from the shape of the query rather than
declared: a bare sum, a bare currency conversion, a bare city name. Exclusivity
stays **off**.

**Augment, not hijack** — the rule that inferred modes add to results instead
of replacing them. Typing `2+2` puts the calculator answer first and still
shows the app you might have been reaching for. This fixes an audited defect in
the old extension, where a bare city name or sum hid everything else. It is why
mode filtering is conditional on `exclusive` and why promotion is conditional
on its negation — the two are mutually exclusive by construction.

**Promotion** — moving an inferred mode's items to the front of the ranked
list, stably, without removing anything.

## Scoring

**Score** = fuzzy match score + kind **weight** + **boost**. Weights break ties
between comparable matches; boosts are strong enough to override match quality.

**Weight** — a per-kind constant expressing which kinds matter more: windows
30, actions and web search 25, apps 20, files 12, emoji 8, the four utility
kinds 6.

**Boost** — an additive nudge for one specific item, from one of two sources,
and their ordering is load-bearing:

- **Alias boost** (`ALIAS_BOOST`, 180.0) — an explicit user instruction.
- **Learning boost** (capped at `LEARNING_BOOST_CAP`, 85.0) — inferred from
  what the user has launched before.

The cap sits strictly below the alias boost so an explicit alias always beats
learned behavior. Both constants are public so that relationship can be
asserted by referencing them rather than repeating their values.

**Frecency** — the learning engine's model: how often an item was launched for
a query, decayed by how long ago. Not "history", not "MRU".

## Result assembly

**Checked items** — provider output that has been checked against the
manifest of the provider that actually produced it: every item's kind is one
its producer declared, and every item's provider string is its producer's
manifest id. Assembly accepts nothing else, so an item's self-description is
never taken on trust.

**Rejection** — one item assembly declined, and which of the two checks it
failed. Returned as data alongside the assembled items, never logged — there
is no logging seam yet, and the query path may not have side effects.

**Ranked body** — the scored, ordered items.

**Pinned tail** — items flagged `append_to_end`, which always follow the ranked
body regardless of score. Web-search actions are the motivating case. They are
split off before ranking and never scored.

**Cap** — the maximum result count, applied to the concatenated body and tail
together. A body that alone fills the cap squeezes the tail out; the old
extension reserved room for the tail instead, and that difference is a recorded
divergence.

## Content rules

**Command-shaped outcome** — an `ExecOutcome` variant that tells a client to
*act* rather than reporting what happened: `CopyText` and `OpenUrl`. Both come
from a provider, so neither is trusted. `Done` is not one, and an item's
`copy_text` is not one either — it reaches the same clipboard, but by way of an
item rather than an outcome.

**Content rule** — a restriction on what a wire value may *contain*, as against
a **bound**, which restricts how long it may be. Content rules live in
`hop-protocol`'s `content` module, bounds in its `limits` module; both are
applied at the deserialization boundary, and the bound is applied first.

**Validating newtype** — a type wrapping a private `String` whose only
constructor applies every rule, and whose `Deserialize` hands the parsed string
to that same constructor. One gate, not two: a value that exists has passed the
rules however it was made. `ItemId`, `ActionId`, `CopyText`, `OpenUrl` and
`QueryText`.

**Allowed scheme** — a URL scheme an `OpenUrl` may carry, from the allow-list
`ALLOWED_URL_SCHEMES`. An allow-list, never a deny-list: a scheme that is not on
it is refused, so a handler the contract has never heard of cannot be reached by
installing a provider that names it.

**Refusal** — a value either gate would not build, named for the rule it broke:
a constructor returns it as an error, and off the parse it becomes an error that
sinks the whole frame. Distinct from a **rejection**, which is an *item* that
assembly declined: a rejection is data returned alongside the items that
survived, so a query with one still answers. Neither is ever a truncation, a
normalization, or a silent fix.

## Redaction

**Redaction** — printing a marker in place of the value, optionally with a
bounded fact about it such as its byte length. Redaction applies to
*formatting*, not to transport: a redacted value is still serialized and sent
whole. A bound restricts how long a value may be, a content rule what it may
contain, and a redaction what formatting it discloses; the three live in
`hop-protocol`'s `limits`, `content` and `redaction` modules. What the disclosed
fact costs is priced on the type that discloses it, under the `# What ... costs`
heading in Conventions, never assumed to be free.

**Redacting newtype** — a validating newtype that also carries its own `Debug`,
so the redaction travels with the value: a field formatted on its own prints the
same marker it prints inside its frame, and a field added to a frame later is
redacted by having the type. `QueryText`, the type of `ClientMsg::Query.text`,
which holds keystrokes typed into the launcher overlay.

## Conventions

**`// DIVERGENCE:`** — the literal marker on any test where behavior
deliberately differs from the old extension's, with the reason inline. Grep for
it to audit every place this codebase knowingly departs from what it ported.
Comments must be self-contained: never defer the justification to a document
outside the repo.

**`# What ... costs`** — the doc-heading form under which a type prices a
decision: what is given up, and the alternative that was rejected instead.
Only `# What` and `costs` are fixed, so the audit is
`grep -rnE '# What .* costs'` rather than a grep for one literal string — and it
is the heading that is greppable, not the wording between. A redaction that
discloses a fact about the value **must** carry one, so that every priced
disclosure is one grep away; anything else that gives something up **may**.
`QueryText`'s `# What reporting the length costs` is the worked example, and
`content`'s `# What refusing a carriage return costs` is the permission taken
up — the same heading spent on a rule's cost rather than a disclosure's.

The form prices what a gate on a wire value gives up, so it is scoped to
`hop-protocol`'s `limits`, `content` and `redaction`. Prose that prices
something else is outside the form rather than missing it: `hop-core`'s
`pipeline` heading `## When the manifest is read, and what that costs` prices
when a check runs, not what a gate discloses or refuses, and the grep above does
not match it.

**Query path** — the code that runs on every keystroke: routing, alias
application, ranking, learning lookup, assembly. Nothing on it may touch disk,
spawn a subprocess, or make a network call. Learning state moves to and from
disk only on explicit `load` and `save`.
