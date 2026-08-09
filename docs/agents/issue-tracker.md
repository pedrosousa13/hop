# Issue tracker: GitHub

<!-- factory:tracker kind=github -->

Issues and PRDs for this repo live as GitHub issues on **pedrosousa13/hop**.

Use the `gh` CLI for all operations. Pass `-R pedrosousa13/hop` explicitly on every
call rather than relying on the current directory's remote — a session that
runs from a worktree, a subdirectory, or another clone stays correct.

## Conventions

- **Create an issue**: `gh issue create -R pedrosousa13/hop --title "..." --body
  "..."`. Title in imperative mood; use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <n> -R pedrosousa13/hop --json
  title,body,labels,milestone,state,stateReason,comments` — `comments` is a
  `--json` field, so one call returns the body and the discussion together.
- **List issues**: `gh issue list -R pedrosousa13/hop --state open --limit 500 --json
  number,title,labels,milestone,createdAt`, plus `--label` / `--milestone`
  filters as needed. `--limit` defaults to 30; pass it on every listing.
- **Comment**: `gh issue comment <n> -R pedrosousa13/hop --body "..."`.
- **Apply / remove labels**: `gh issue edit <n> -R pedrosousa13/hop --add-label
  "..."` / `--remove-label "..."`.
- **Close**: `gh issue close <n> -R pedrosousa13/hop --reason completed` (resolved)
  or `--reason "not planned"` (wontfix).

## When a skill says "publish to the issue tracker"

Create a GitHub issue on pedrosousa13/hop.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <n> -R pedrosousa13/hop --json
title,body,labels,milestone,state,stateReason,comments`.

## Factory loop operations

GitHub's answer to each row of the tracker contract in `PROTOCOL.md`, the
Factory plugin's own protocol document — a session with the Factory
installed can find it, and one without it has no use for this section — one
bullet per row. A `/factory` Loop Session needs every one of them.

- **Reachability**: `gh auth status` resolves the `gh` CLI and confirms it
  is authenticated; `gh repo view pedrosousa13/hop --json name` confirms the
  **pedrosousa13/hop** repo exists and is visible to this account.
- **Queue listing**: `gh issue list -R pedrosousa13/hop --state open --label
  ready-for-agent --milestone <n-or-title> --limit 500 --json
  number,title,labels,milestone,createdAt`. `--milestone` accepts either a
  milestone number or its title; drop the flag entirely for an unscoped
  run. `--limit` is not optional: `gh issue list` fetches 30 by default, so
  a Queue longer than that loses everything past the cap with no error, and
  because the order below is applied to whatever came back, the issue that
  should have been picked can simply be absent. Unstarted means the issue
  does **not** carry `in-progress` — GitHub has no started state, so the
  label stands in for one (see "Where a label is weaker than a field"
  below). Treat the result as a set of candidates to confirm, not as fact:
  the listing lags label writes, and each candidate is re-checked
  individually before it is picked.
- **Queue order**: the `P0`–`P3` labels, highest first — **`P0` (Urgent) >
  `P1` (High) > `P2` (Medium) > `P3` (Low) > no priority label** — ties
  broken by the oldest `createdAt`. Both the labels and `createdAt` come
  back on the same listing call, so ordering costs no extra call.
- **State: started**: `gh issue edit <n> -R pedrosousa13/hop --add-label
  in-progress --add-assignee @me` — one call, which is what makes pickup
  atomic. GitHub issues have only `OPEN` and `CLOSED`, so `in-progress` is
  the started state.
- **State: completed / canceled**: `gh issue close <n> -R pedrosousa13/hop --reason
  completed` for landed work, which reads back as `state=CLOSED`,
  `stateReason=COMPLETED`. Wontfix is two calls, because `gh issue close`
  has no label flag: `gh issue edit <n> -R pedrosousa13/hop --add-label wontfix`,
  then `gh issue close <n> -R pedrosousa13/hop --reason "not planned"`, which reads
  back as `state=CLOSED`, `stateReason=NOT_PLANNED`. The reason is what
  distinguishes the two — a closed issue with no reason is
  indistinguishable from either.
- **Park**: `gh issue edit <n> -R pedrosousa13/hop --remove-label ready-for-agent
  --remove-label in-progress --add-label needs-info`. The issue stays
  **open**: Park returns work to an unstarted state, it does not close it.
  Removing `in-progress` is the unstarted half of the Park and is not
  optional — an issue left carrying it never re-enters the Queue even once
  it is re-labeled `ready-for-agent`.
- **Blocking**: the `## Blocked by` **section** in the issue body is the
  **single canonical mechanism** — not GitHub's native sub-issues, which
  this `gh` cannot use in either direction (see below), and not the older
  inline `Blocked by #N` line a few **closed** issues still carry (see the
  fail-safe below, not a second parser to maintain). Issues filed from a
  plan carry:

  ```markdown
  ## Blocked by

  - #53
  ```

  An issue with no blockers writes `None - can start immediately` under the
  same heading, so the heading's presence alone is **not** a blocker signal.

  Parse it with this five-step algorithm, in order — each step exists
  because a naive version of it was tried and watched to fail:

  1. **Strip fenced code blocks from the body before scanning anything.** A
     `## Blocked by` heading can appear inside a fenced code block as an
     example — issue #96's own body does exactly that, quoting the section
     form inside a fenced snippet while its real section, near the end of
     the body, says `None - can start immediately`. A parser that scans raw
     text without stripping fences reads the fenced example as if it were
     the real section and reports #96 as blocked by a long-closed issue.
  2. **Find the `## Blocked by` heading.** More than one surviving
     fence-stripping is ambiguous — treat the issue as **blocked** and
     report it, never as clear.
  3. **Skip blank lines after the heading, then read contiguous list items
     only** (`-` or `*`). Stop at the first line that is not a list item.
     Every `#N` in those items is a blocker; nothing outside them is. This
     matters because prose after the list can itself name the issue's own
     number: #103's section reads `- #57` followed by an explanatory
     paragraph that happens to mention "#103" — a parser that keeps reading
     "every `#N` until the next `##`" invents a phantom self-blocker from
     that paragraph. Stopping at the first non-list-item line avoids it.
  4. An issue is blocked while any `#N` found this way is still open.
  5. **A non-zero exit or a parse failure anywhere in this path means
     blocked, not clear.**

  Read the body with `gh issue view <n> -R pedrosousa13/hop --json body`;
  `gh issue list` doesn't return it, so each candidate needs its own `gh
  issue view` — check them one at a time, in Queue order, and stop at the
  first unblocked one.

  **Convergence fail-safe.** The section form above is the only one this
  adapter documents, but it is not the only one the repo has ever used.
  Seven **closed** issues (#21, #26, #28, #29, #30, #32, #34) carry a
  legacy bold inline `**Blocked by #N**` line instead, and are deliberately
  **not** migrated to the section form — Queue selection only ever reads
  open issues' bodies, so nothing ever parses a closed issue's, and these
  should not be mistaken for work left undone. An **open** issue is a
  different matter: if one is ever found carrying `Blocked by #N` text
  outside the canonical section — the legacy inline form, or anything else
  shaped like it — treat that issue as **blocked**, or stop and report it
  for attention, never silently ignore it just because the five-step scan
  above found nothing. This is what keeps one documented parser safe
  without maintaining a second one forever: a stray blocker outside the
  canonical section still fails toward "blocked," not toward "the scan's
  silence means clear."

  **Sub-issues do not work on the installed `gh` (2.78.0, 2025-08-21), in
  either direction.** The read this adapter used to document —
  `gh issue view <n> -R pedrosousa13/hop --json subIssues,subIssuesSummary` —
  exits **1** with `Unknown JSON field: "subIssues"`; neither that field nor
  `parent` appears anywhere in `gh issue view --json`'s field list on this
  version. That is a hard failure, not an empty result — a loop that ignores
  the exit code, or pipes the output through something like `tail`, reads it
  as "no blockers" and proceeds. The read that does work is the REST
  endpoint, `gh api repos/pedrosousa13/hop/issues/<n>/sub_issues`, which
  returns `[]` for every one of this repo's issues — all 72 of them, open
  and closed, checked exhaustively rather than sampled. The sub-issue
  relation is entirely unused here. Writing one is equally impossible: `gh issue edit
  --help` has no `--add-sub-issue` flag on this `gh`, so there is no command
  that creates the edge even deliberately. A session that needs to express a
  blocking relationship must write it into the `## Blocked by` section of
  the issue body instead — the same convention `/to-tickets` uses — never
  reach for a sub-issue write, which does not exist on this `gh`.

  **The body section is authoritative.** No issue in this repo carries a
  sub-issue edge and none can be written, so the body is the only place a
  blocking relation is ever actually expressed; if the two mechanisms were
  ever both populated, the body governs.

  **The fail direction is the single most important fact in this bullet: a
  blocking-parse failure must fail toward "blocked," never toward
  "unblocked."** A non-zero exit from either read above, a body with more
  than one `## Blocked by` heading surviving fence-stripping, or any other
  result that does not parse cleanly all mean treat the issue as blocked and
  move to the next Queue candidate — never treat a failed or incomplete
  parse as evidence that no blocker exists. Misreading a blocked issue as
  unblocked lets a session start work whose foundation does not exist yet;
  misreading an unblocked issue as blocked only costs one skipped candidate,
  which Queue order already recovers from on the next check.
- **Milestone**: a GitHub **milestone** on the issue, not a label. Create
  one with `gh api repos/pedrosousa13/hop/milestones -f title=... -f
  description=...`; list a repo's milestones with `gh api --paginate
  "repos/pedrosousa13/hop/milestones?state=all&per_page=100"`, which returns them
  in GitHub's own order, stable between runs. Both halves of that query
  are load-bearing: the endpoint returns only open milestones by default
  and pages at 30, and the milestone menu is supposed to show *every*
  milestone in the Project — a stable menu shape matters more than hiding
  the closed or the empty ones. `--milestone` does not take the same value
  on every subcommand, though: `gh issue list --milestone` and `gh issue
  edit --milestone` both accept a milestone number or its title, but `gh
  issue create --milestone` accepts the **title only** — `gh issue create
  --help` documents the flag as "by name", with no mention of a number, and
  a maintainer's own test confirms it: passing a number fails with `could
  not add to milestone '<n>': '<n>' not found` and creates **no** issue, so
  the failure is safe to retry, but the message reads as though only the
  milestone step failed rather than the whole call. Set one with `gh issue
  create --milestone <title>` at creation, or `gh issue edit <n> -R
  pedrosousa13/hop --milestone <n-or-title>` afterwards. Read a milestone's
  completion with `gh api
  repos/pedrosousa13/hop/milestones/<n>` and its `open_issues` / `closed_issues`
  counts — GitHub reports no percentage, so compute one from the pair.
- **Milestone issue counts**: `gh issue list -R pedrosousa13/hop --state all
  --milestone <n> --limit 500 --json number,labels,state,stateReason`,
  bucketed by state: **done** is `state=CLOSED` with
  `stateReason=COMPLETED`; **canceled** is `state=CLOSED` with
  `stateReason=NOT_PLANNED`; **started** is `state=OPEN` carrying
  `in-progress`; among the rest (`state=OPEN`, no `in-progress`), a
  `needs-info` label makes the issue **parked**, its absence makes it
  **unstarted**. `--limit` is not optional here either: leave it off and
  the default of 30 pins every larger milestone at exactly 30, and the
  empty-Queue report states a wrong number without any sign that it did.
  This is deliberately not a re-count of the Queue, which sees only
  `ready-for-agent`.
- **Open issues**: `gh issue list -R pedrosousa13/hop --state open --milestone
  <n-or-title> --limit 500 --json
  number,title,labels,milestone,createdAt,assignees,body`. Drop
  `--milestone` entirely for an unscoped call. `body` replaces the
  `subIssuesSummary` field this bullet used to request — that field is
  unknown to `gh issue list --json` on the installed `gh` and makes the
  whole call exit 1, so it must not appear here or anywhere else in this
  doc. Every open issue, unfiltered by label, unlike Queue listing above.
  Full ticket facts per issue: `state` derived as under **Milestone issue
  counts** above, `blockedBy` from the same body check as **Blocking**
  above, `claimedBy` from `assignees`.
- **Read an issue**: `gh issue view <n> -R pedrosousa13/hop --json
  title,body,labels,milestone,state,stateReason,comments` — one call.
  `comments` is a valid `--json` field and returns each comment's author,
  body and timestamp, so the body and the whole discussion come back
  together. `gh issue view <n> -R pedrosousa13/hop --comments` renders the same
  discussion for a human to read, but a session working from the issue
  needs only the `--json` call.
- **Comment**: `gh issue comment <n> -R pedrosousa13/hop --body "..."`. Body as
  Markdown; use a heredoc so newlines stay literal.
- **Branch name**: GitHub supplies none, so it is a convention this repo
  derives: `<user>/issue-<number>-<slug>`, where `<user>` is the
  maintainer's GitHub login and `<slug>` is the issue title lowercased,
  non-alphanumerics collapsed to single hyphens, trimmed to a few words —
  e.g. `pedrosousa13/issue-42-add-the-github-adapter`. Nothing stores it,
  so every session that touches the issue must derive it the same way from
  the same title, and a session resuming an issue looks for that branch
  rather than inventing a new one.
- **State verification**: `gh issue view <n> -R pedrosousa13/hop --json
  state,stateReason,labels,milestone` returns the issue's current state.
  Fetch it fresh when verifying a Pause note's claim — never compare
  against a value read earlier in the session.

## Wayfinding operations

GitHub's answer to what the `/wayfinder` skill (`~/.claude/skills/wayfinder`)
needs from a tracker. Wayfinder maps and their tickets are planning
artifacts, not work items: they carry `wayfinder:*` labels in place of the
triage axes, never `ready-for-agent` and never `in-progress`, so they can
never enter a Loop Session's Queue ("Wayfinder maps" in `PROTOCOL.md`, the
Factory plugin's own protocol document — not a file in this repo;
"Wayfinder labels" in `docs/agents/triage-labels.md`).

- **The map**: an ordinary issue on **pedrosousa13/hop** labeled `wayfinder:map`.
  Find a Project's maps with `gh issue list -R pedrosousa13/hop --state open
  --label wayfinder:map --limit 500 --json number,title`.
- **Labels**: `wayfinder:map`, `wayfinder:research`, `wayfinder:prototype`,
  `wayfinder:grilling`, `wayfinder:task` — repo labels on **pedrosousa13/hop**,
  created lazily by the first charting session: `gh label list -R pedrosousa13/hop`
  first, then `gh label create <label> -R pedrosousa13/hop` only for the names that
  are missing. Never create a label you haven't first confirmed is missing.
- **Child tickets**: GitHub's native sub-issues would be the natural fit —
  and this bullet used to document `gh issue edit <map> --add-sub-issue
  <ticket>` for it — but that relation is **unavailable** on the installed
  `gh` (2.78.0): `gh issue edit --help` has no `--add-sub-issue` flag, so
  there is no command that creates it, and `gh api
  repos/pedrosousa13/hop/issues/<n>/sub_issues` confirms none exist today.
  Express map→ticket parentage the same way the **Blocking** bullet above
  expresses blocking — in the ticket body — by naming the map's issue
  number in the ticket (e.g. a `Part of #<map>` line). This is the same
  fallback the wayfinder skill's own doc anticipates for a tracker that
  lacks a working native relationship; it just also applies to parentage,
  not only blocking, on this `gh`.
- **Blocking between tickets**: a sub-issue has exactly one parent and the
  map holds that slot, so ticket-to-ticket edges use the same body
  convention the loop's **Blocking** bullet reads — a `## Blocked by`
  section in the blocked ticket's body, added in a second pass once every
  ticket has a number. Parse and fail-direction rules are identical to that
  bullet's: five-step canonical parse, fail toward blocked on any ambiguity
  or parse failure, and the same convergence fail-safe for stray
  `Blocked by #N` text outside the section.
- **Frontier**: do **not** reach for `--json subIssues` here. It is an
  unknown JSON field on this `gh` and exits 1, the same hard failure as the
  loop's **Blocking** bullet describes, and the working REST read
  (`gh api repos/pedrosousa13/hop/issues/<n>/sub_issues`, per that bullet)
  returns `[]` for every issue in this repo — so even the call that *does*
  run has no child relation to report. Enumerate candidates instead from
  the body-reference convention in
  **Child tickets** above: `gh issue list -R pedrosousa13/hop --state open
  --limit 500 --json number,body`, then keep the ones whose body names the
  map (`#<map>`). Confirm each candidate with its own `gh issue view <n> -R
  pedrosousa13/hop --json assignees,body,state` — unclaimed means no
  assignee; unblocked means no `Blocked by` text naming a still-open issue,
  read per the loop's **Blocking** bullet. The per-candidate view is the
  authority here for the same reason it is in Queue selection: the listing
  lags.
- **Claim**: `gh issue edit <n> -R pedrosousa13/hop --add-assignee @me` — the
  assignee is the claim; an open, unassigned ticket is unclaimed.
- **Resolve**: post the resolution with `gh issue comment`, then `gh issue
  close <n> -R pedrosousa13/hop --reason completed`. A ticket ruled out of scope
  closes with `--reason "not planned"` instead — resolved and ruled-out
  stay distinguishable, the same way landed and wontfix do.

## Reachability

What the Factory's Preflight checks: `gh` resolves and is authenticated,
and the **pedrosousa13/hop** repo exists and is visible — `gh auth status`, then
`gh repo view pedrosousa13/hop --json name`.

## If GitHub is unreachable

Say so and stop. Don't silently fall back to another tracker or local files.

## Where a label is weaker than a field

Linear-shaped trackers carry state and priority as native fields; GitHub
carries them as labels, which nothing validates. Three invariants can
therefore break. Each has a resolution rule, so two sessions over identical
state still behave the same.

- **Two priority labels on one issue.** Nothing stops `P0` and `P2` both
  being applied. Rule: **highest wins** — `P0` beats `P1` beats `P2` beats
  `P3`. Ordering stays deterministic no matter how many priority labels an
  issue carries, and no session has to stop and ask.
- **`in-progress` is enforced by nothing.** A session that dies mid-issue
  leaves the label behind; nothing removes it. Rule: `in-progress` on an
  issue with no matching branch (see **Branch name** above) is a **stale
  marker to verify, not a fact to trust** — the same posture `PROTOCOL.md`'s
  Pause note section takes toward an interrupted state. Verify against the
  branch and the Pause note; if neither backs it up, the issue was never
  really started.
- **`gh issue list` lags label writes.** Verified against a real repo:
  freshly created issues were missing from a filtered listing for tens of
  seconds, and an issue kept appearing in a `--label ready-for-agent`
  listing for about a minute after that label was removed. `gh issue view`
  on the same issue was correct immediately, every time. Rule: **the
  listing is a hint; the per-candidate `gh issue view` is the authority.**
  Queue selection already confirms each candidate individually for the
  blocker check — that same confirmation must also re-check that the
  candidate still carries `ready-for-agent`, still lacks `in-progress`, and
  is still open, and skip it otherwise. Without that re-check, a Loop
  Session that Parks an issue and immediately re-runs Queue selection
  re-picks the issue it just Parked, Parks it again, and loops forever.
