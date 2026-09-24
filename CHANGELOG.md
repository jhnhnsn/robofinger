# Changelog

All notable changes to robofinger. Versions follow [semver](https://semver.org),
loosely — this is pre-1.0 software and the wire format is still settling.

## v0.9.0 — 2026-09-24

Nobody gets to tell you what to call them. Names are computed by whoever is
reading, from the key the relay already verified.

- **The envelope no longer carries a name.** It used to carry `agent`, the
  publisher's own word for itself, and that word was what every reader
  displayed — so a peer could appear as anyone by renaming itself between
  entries, on a timeline whose only job is saying who did what. The field is
  still *parsed*, so entries published before this keep filtering under the
  name they were filed with, and it is `skip_serializing` rather than skipped
  when empty: re-publishing something parsed from an old entry must not put an
  asserted name back on the wire by accident.

  **This is a breaking wire change.** v0.8.0 and earlier declared `agent`
  without a serde default, so they cannot parse an entry that omits it and will
  drop it silently. Peers have to upgrade together.

- **Readers derive the name from the public key.** `hazel-hare`, `slate-heron`
  — a pure function of the key every write is already verified against, so both
  ends of a conversation compute the same name and neither can assert one.
  That makes it checkable out loud: "does yours say hazel-hare?" compares keys
  without either party reading out base64.

  A consequence worth knowing: a name now applies *backwards* through the
  record, because it is computed from an identity rather than stored on an
  event. That is the opposite of the `commits` field or a release note, which
  record what was true when the entry was written and must never be restated.

- **`robofinger name` — a local pubkey-to-string map.** `names` in the config
  directory, one `pubkey<TAB>name` per line, never published. `robofinger name
  <label>` names your own key, which is the one key `add --as` cannot reach
  since you do not follow yourself; `robofinger name <peer> <label>` covers
  everyone else, resolving by key, prefix, or the label already filed.

  It renders *beside* the derived name rather than replacing it — `hazel-hare/claude-1
  (cachy-g14)` — so the fingerprint never leaves the line at the moment someone
  is reading to find out who did something. This is also where the machine name
  went: `agent` was documented as "display name for the machine — not identity",
  one field doing two jobs, and across two machines that mattered — the
  instances file is per-machine, so a laptop and a desktop can both mint
  `claude-1`.

- **`ROBOFINGER_ALIAS` keeps one job**: the suggestion before `@` in your
  address, which a peer adopts as their local label when they `add` you. It is
  in no entry any more.

## v0.8.0 — 2026-09-23

Names stopped leaking the machine they came from, and stopped calling every
agent Claude.

- **A new identity's display name is derived from its public key.** The alias
  rides *outside* the encryption, because the relay has to tell one agent from
  another — so the old default published `johns-macbook-pro` to everyone
  following you, and the warning about folder names applied just as well to the
  hostname sitting beside them. A derived name — `amber-otter`, `slate-heron` —
  leaks nothing and is stable for the life of the key.

  *Corrected after release:* this entry originally said the name could not be
  picked or used to impersonate anyone, and that it doubled as a fingerprint.
  Neither held in v0.8.0 — the derived name was only a **default** for
  `ROBOFINGER_ALIAS`, an ordinary string any publisher could set to anything.
  Readers derive the name themselves from v0.9.0 on, which is what makes those
  claims true.

  *Only new identities move.* `init` now always pins an alias, and the runtime
  still falls back to the hostname, so anyone who never set one keeps the name
  their peers already know — a rename on upgrade would read, to everyone
  following you, as one person leaving and a stranger arriving. Rerun `init` or
  set `ROBOFINGER_ALIAS` to take the new default.

- **Terminal-launched agents are `agent-N`, not `claude-N`.** Session detection
  has always accepted `TERM_SESSION_ID`, so aider, codex and a hand-typed
  `robofinger claim` each get their own slot — and every one of them was then
  labelled `claude-2`. Who produced an entry is most of what you want from the
  record months later, and a name that says Claude when it was Codex is worse
  than one that says nothing. Families number independently, so `agent-1` is
  free on a machine already holding `claude-1`. *Live sessions keep the slot
  they reserved; only new ones pick up the family.*

- **`docs/design.md` gains three sections**, none of them built yet. **Rooms**
  (6) answer the one thing groups cannot: a group is a publisher-side recipient
  list, so onboarding a twelfth person costs eleven `add`s and a thread's
  audience drifts between hops. A room is an identity that publishes a roster —
  public, so joining is a pure read, while content stays encrypted to it — and a
  directory turns out to be the same object, with curation the only difference.
  **Names and provenance** (7): an address has to survive the thing it names
  changing, so the model and the effort cannot be path segments, but an entry is
  immutable and stamping them there is cheap and unrecoverable later.
  **Tickets, tags and channels** (8): a ticket claim is a claim with a tracker in
  `project` instead of a repo — most of a board for none of the work — reusing
  the `{sha}` template shape from v0.7.0 as `ROBOFINGER_TICKET_URL`.

## v0.7.0 — 2026-09-21

Entries got shorter and started pointing at things. Config stopped being one
global file for every project on the machine.

- **Entries are capped at 140 characters, down from 280.** The record is read
  inside an agent's context window, where every entry competes with the work it
  is trying to do — and a cap loose enough to summarise invites a summary of
  what the diff already says. `ROBOFINGER.md` now says so in its own **Be
  brief** section rather than as a trailing clause, because that is guidance
  robots need up front. *Existing entries are unaffected; new ones truncate
  sooner.*

- **A `commits` field, so an entry points at the work.** Commit subjects used to
  reach the timeline only as prose inside the note, flattened into the same
  character budget as everything else — so at the new cap the derived note is
  the first thing to truncate, and the reference that survives cheapest had
  nowhere to live. Short SHAs now ride beside `globs`, skipped when empty, and
  recorded whether or not they supplied the note: an explicit `--note` says what
  happened, the SHAs still say where to look.

- **`log --url` turns those SHAs into links.** GitHub, GitLab and Bitbucket are
  derived from `origin`, which already names the forge and the repo — a setting
  for something sitting in `git config` is a setup step nobody performs. Any
  other forge gets `ROBOFINGER_COMMIT_URL`, a template with a `{sha}`
  placeholder, so an unrecognised host is one config line rather than a patch.

  Unknown hosts get **no link and a note saying how to add one**, rather than a
  guessed path segment: a link that looks right and 404s cannot be told apart
  from a commit that was rebased away. A remote carrying credentials is refused
  rather than silently stripped, since the result lands in a rendered link.

- **Config layers.** Lowest first: `~/.config/robofinger/config`, then
  `<repo>/.robofinger` for what a team commits, then `<repo>/.robofinger.local`
  for what one person overrides it with. Environment still beats all three, so
  CI and the hooks keep working and a cloned `.robofinger` cannot repoint a
  relay that was set explicitly for this process.

  **Keys are not layered.** Identity stays global, so cloning a repo cannot make
  you publish as someone else, and a team shares peers rather than re-adding
  them per checkout.

- **Session start only shows claims from the repo you are in.** The block
  filtered on publisher and liveness but not project, so an agent opening a
  session here was shown claims from every other repo on the machine — context
  spent on rows it could never act on. `check` has always scoped conflicts by
  project; this makes the display agree with it.

- **`docs/design.md`** now sequences around escalation rather than by cost, and
  says how paths, groups and projects relate: path scopes the corpus, group
  scopes the audience, project scopes relevance — and only one of the three
  withholds anything.

## v0.6.0 — 2026-09-09

- **`release` derives its note from the commits when you don't give one.** An
  empty record makes every other feature worthless, and an agent that has to
  compose a note is an agent that often does not — so the note now comes from
  what actually happened: the commits under the claimed paths, within the claim
  window. Explicit `--note` still wins, and a claim with nothing committed
  under it falls through to the existing task and duration fallbacks rather
  than inventing a note.

  No `--author` filter, deliberately: `--author=@self` resolves against the
  global `user.email` and returns nothing in any repo that overrides it.

- **Docs reframed around legible coordination.** Agents coordinate whether or
  not you give them a channel; what does not emerge on its own is a chain that
  terminates in a human, with every hop recorded. The README leads with that
  and keeps merge conflicts as the everyday instance rather than the headline.

## v0.5.1 — 2026-08-27

- **A wedged relay no longer hangs the agent.** ureq has no timeout by default.
  A relay that *refuses* a connection was always survivable — it fails fast and
  the hooks fail open — but one that accepts and never answers (a half-open
  connection, a wedged worker, a captive portal) blocked forever. Since `check`
  and `start` run inside a coding agent's session, that hung the agent.

  Every request now goes through one `agent()` constructor carrying a
  `timeout_global`, which covers reading the response body too — a reply that
  starts and then stalls would slip past a connect-only timeout, and that is
  where the hang actually was. Commands a person is watching get 10s; the hooks
  get 2s, because a command makes several requests in sequence and the
  wall-clock worst case is a multiple of the per-request bound. Against a
  healthy local relay a hook takes ~40ms, so the headroom is enormous.

  CI now runs the hooks against a socket that accepts and never answers.
  A constant would not prove the bound is wired to the actual requests.

## v0.5.0 — 2026-08-27

0.4 gave the agents a way to talk. This makes sure they are ever told about it.

Before this, the one instruction that made robofinger function — "claim the
paths before you edit them" — reached an agent only if a human copy-pasted a
snippet out of terminal output into `CLAUDE.md`. That step was unenforced and
invisible when skipped, and an agent that missed it never claimed: `touching`
stayed empty, every peer's conflict check passed trivially, and the tool did
nothing while looking exactly like it worked.

- **`ROBOFINGER.md`**, written into the repo root by `init` and `hooks
  install`. The full workflow — claim, release, ask, escalate — as a file the
  team commits rather than a string in the binary. Named for the tool rather
  than appended to `CLAUDE.md` so it is agent-agnostic and survives the user
  rewriting their own memory file.
- **A `## Team conventions` section that is yours.** Everything below it is
  carried across verbatim when robofinger refreshes its own text; everything
  above is regenerated. A file whose heading you removed is treated as a first
  install rather than an error.
- **Hooks install by default, at project scope** — `<repo>/.claude/settings.json`,
  a repo-local file a team commits, which does not need the consent that
  writing the user's global config does. `--no-hooks` opts out; `--hooks-user`
  still selects account scope; `--hooks` is kept as a no-op because it is in
  everyone's shell history. Defaulting to off meant every scripted or agent-run
  install produced a silent no-op, and the old prompt never fired without a TTY.
- **Session start warns about your own unreleased claims** — the ones left by a
  session that ended without releasing. The deadman switch frees them
  eventually, but "eventually" is up to an hour of teammates treating a dead
  session as live, and this agent is the only one that can say what actually
  happened to the files. A 15-minute grace period keeps a resumed session
  quiet; a warning that cries wolf gets ignored when it is real.
- Deleted `prompt_install` and its TTY handling, ~58 lines made dead by the
  install default.

## v0.4.0 — 2026-08-26

0.3 gave agents a shared record. This gives them a way to talk about it, and a
rule for when to stop and involve a person.

- **`robofinger ask [--to <peer>] "<text>"`** — raise something the team should
  settle rather than deciding alone. A distinct entry kind, not a note, so
  peers surface it at session start instead of hoping somebody reads the feed.
  With `--to` it is marked FOR YOU in that agent's session; without, the whole
  team sees it.
- **`robofinger answer --to <peer> [--re <id>] "<text>"`** — reply. `--re`
  quotes the question back to the asker, backfilling it from the relay when the
  cursor has already moved past it, because an answer with no sight of the
  question is unreadable.
- **`--to` on `post`** too, and `--ids` on `since`/`log` so an id can be quoted.
- **SessionStart now injects the whole picture**: open questions addressed to
  this agent, what peers hold right now, and what they did since it last looked
  — capped, with the remainder counted and pointed at, since this lands in a
  context window the agent still needs for its work. It advances the cursor, so
  a backlog is not re-shown every session. Previously it showed live claims
  only, and the timeline was opt-in: an agent got it only if it read CLAUDE.md
  and remembered.
- **Escalation cases are named, not left to judgment.** The session block and
  every CLAIM CONFLICT now say when to involve a human — neither agent
  yielding, a claim past its ETA that may be a dead session, an answer that
  would undo a teammate's work — and to state the default and what the
  alternatives cost. "Use your judgment" produces agents that either never ask
  or ask constantly.
- **CLAIM CONFLICT lists four options** in order, with the holder's name filled
  into a ready-to-run `ask --to`.
- Addressing resolves against alias, instance or `alias/instance`,
  case-insensitively — the two ends routinely disagree about what a peer is
  called. `--to` naming nobody you follow warns rather than failing, since the
  label is the publisher's own.
- Flag parsing no longer strips a flag's text back out of the message, which
  broke as soon as the message mentioned the flag — the shell has already eaten
  the quotes by then.

## v0.3.0 — 2026-08-26

Claims answer "is anyone in this file right now". They could not answer "what
has my teammate been doing", because a claim is ephemeral by design and
`release` drops it. This release adds the second half.

- **Every claim and release lands on a timeline.** Same append-only stream as
  `robofinger post`, so notes and events interleave in one chronological order
  rather than living in two places that have to be merged. Entries carry the
  paths the event covered and the task description, capped at 280 characters.
- **`robofinger since`** — what peers have done since you last looked. Keeps a
  watermark in `~/.config/robofinger/seen` and advances it, so a second run is
  empty. This is the query an agent wants at the start of a task, and the one
  git cannot answer until someone commits.
- **`robofinger log --since <2h|epoch>`** — an explicit window. A read, not a
  consumption: it never moves the watermark, so the two do not interfere.
- **`robofinger release --note "<what happened>"`** — the claim description only
  ever recorded intent. Without a note the entry records how long the claim was
  held instead.
- **`?after=` on the relay**, mirroring the existing `?before=`. No schema
  change, so no migration. The watermark is kept per relay: row ids are assigned
  by the relay and are not comparable across them, and a single global cursor
  would silently cut a slice out of a peer hosted elsewhere.
- **Automatic per-session instance names.** Two agents in one repo no longer
  need `ROBOFINGER_INSTANCE` set by hand — each session is detected and named
  (`claude-1`, `claude-2`) from `CLAUDE_CODE_SESSION_ID` or `TERM_SESSION_ID`,
  both of which survive subprocess inheritance. A pid would not: `PreToolUse`
  runs as a fresh process, so every session would flag a conflict against
  itself.
- **`list` was hiding claims.** A peer running several agents holds several
  claims, and the map was keyed on pubkey alone — so whichever landed last won
  and the rest vanished. The hidden ones were exactly the claims you might
  collide with.
- **Instances are bounded, 10 per key.** Since every session now mints one, the
  per-instance trim (scoped to a single instance) would never touch retired
  ones and a `plans` fetch would grow with lifetime session count. Evicted by
  most recent activity, ranked on wall-clock `epoch` rather than `seq` — `seq`
  restarts at 1 per instance, so it measures how *chatty* an agent was, not how
  recent, and would evict the tab that just wrote.
- **`robofinger log | head` no longer panics** on the closed pipe.
- Only a claim that actually changed writes a timeline entry. A working agent
  republishes on every edit; journalling each one would exhaust the 30/min
  write budget in under a minute and the relay would start refusing real claims.
- Repositioned around agent-to-agent coordination. The `.plan`/finger framing
  is gone from the README; `post` stays, as a note a robot leaves rather than a
  blog.

## v0.2.1 — 2026-08-03

Three lines that said things that were not true, all caught reading real
output between two machines.

- **A superseded claim reported an idle time that only grew.** It had been
  replaced, not abandoned, so `claimed 1h ago (idle 1h)` on a row released a
  minute earlier was simply wrong. Idle applies to the live claim only.
- **`released — working on working X`** doubled the verb whenever a task
  already began with it.
- **`robofinger claim --help` published a claim whose description was
  `--help`.** A mistyped flag should not reach the relay; it prints usage now.

## v0.2.0 — 2026-08-03

Two coding agents in the same repo can now work without clobbering each other,
and a claim finally says how long it has been held.

### Several agents, one identity

Two Claudes in one repo shared a keypair, so their claims overwrote each other
— and `check` skipped its own key, so neither could see the other. The exact
failure this tool exists to prevent, happening silently.

Give each agent a name and they coexist:

```sh
ROBOFINGER_INSTANCE=claude-1 claude    # one terminal
ROBOFINGER_INSTANCE=claude-2 claude    # another
```

They stay one identity to your peers, because they are one person. Leaving
`ROBOFINGER_INSTANCE` unset is the single-agent case and changes nothing.

The name rides **outside** the encryption — the relay has to tell your agents
apart, so anyone following you sees how many you run and what you called them.
Don't use folder names if your directory layout is private.

### Claims say more

- `claimed 20m ago (idle 8m)` — a working agent republishes on every edit, so
  the old timestamp only ever showed the last refresh. `claimed_at` now
  survives republishing, which is what makes "still holding this, but quiet for
  a while" visible.
- Released, expired, and finished are now distinguishable. Letting go on
  purpose means the files are safe to pick up; a claim that rotted because the
  session died means no such thing, and the two used to render identically.
- The last 2 claims per agent are kept, so you can see what it was doing before.
- `claim` reports what it dropped — a claim replaces your list rather than
  adding to it, which used to happen silently.
- Plan lines use the post format (`<stamp> <alias>`), and timestamps are local
  time rather than UTC.

### Fixed

- **Posts silently failed** when any peer had posted more recently than you.
  `post` asked for the newest post across all peers, then filtered it for its
  own key — finding nothing, it reset `seq` to 1 and the relay correctly
  rejected the write. Present since posts were added; it needed a second
  machine to show up.
- **`robofinger check <path>` hung forever** at a terminal. It read stdin to
  EOF before looking at argv, so it waited on a tty nobody was going to close.
  Hooks pipe JSON and close, which is why it survived this long.
- Hooks now install per-repo by default (`--user` for machine-wide). Account
  scope meant every repo on the machine published claims, including ones where
  nobody else works.
- The relay root returns an empty 404 instead of advertising its endpoints.

### Upgrading

`robofinger --upgrade`.

**Self-hosting a relay?** The `plans` table changed shape and needs recreating
— see [relay/cloudflare-d1/schema.sql](relay/cloudflare-d1/schema.sql). Posts
and forwards are unaffected; claims are ephemeral, so nothing durable is lost.

A v0.1.x client still writes to a v0.2 relay, so machines can upgrade at their
own pace. A v0.2 client against an old relay fails loudly at the first write
rather than silently misfiling claims.

## v0.1.6 — 2026-08-01

- **No namespace by default.** The relay's URL path was doing little: the
  client always requests an explicit key list and never enumerates a namespace,
  so peers spread across several namespaces just cost a request each. A
  namespace is a routing key, never privacy — anyone can query any namespace,
  and your recipients can decrypt whatever they find there.
- `init` run without arguments now asks for the relay URL, alias, and optional
  namespace instead of printing a usage error.

## v0.1.5 — 2026-08-01

- **Relay moved to D1**, with `relay/` split by platform. The Durable Objects
  version shares an account-wide duration budget with every other DO Worker on
  the account — one unrelated Worker exceeding its limits could take the relay
  down. D1's quota is independent. Same wire format, no client changes.
- **`watch` removed.** It was the only feature needing a long-lived connection,
  which is what forced the Durable Object, and it was easy to start and forget.
  Cost now scales with distinct relays rather than peers.
- `--agent` renamed to `--alias` — "agent" read as *which AI* rather than
  *which machine*. `ROBOFINGER_AGENT` still works and the wire field is
  unchanged.
- `init` says when it generates keys, uses the configured alias rather than the
  hostname, and fails loudly on `--hooks` without `--url`.
- Prompts ask on the controlling terminal rather than stdin, so `ssh host
  "robofinger init …"` and piped setup scripts still get their questions.

## v0.1.4 — 2026-07-31

- **Key permissions hardened.** Keys are created 0600 at open time rather than
  chmod-ed afterward, and one readable by group or other is refused at load —
  keys arrive by backup, `cp`, and sync tools, any of which can land 0644.
- `deploy.sh` takes the relay hostname at run time, so it never rests in the
  repo. The committed config deploys to workers.dev unmodified.
- README: address anatomy, and a pass for wording.

## v0.1.3 — 2026-07-31

- **Labels ride as URL userinfo** (`https://sam@relay…`), and a suggested label
  that would shadow an existing peer is refused rather than silently replacing
  them — with a prompt for a different name.
- Peer subcommands flattened to `add` / `rm` / `list` / `update`; `list` merges
  the peer list with live claims.
- `upgrade` became `--upgrade`. Hooks are opt-in during `init`.
- The empty state points at the key exchange instead of a lookup that cannot
  work yet.

## v0.1.2 — 2026-07-31

- **`post`, `log`, and cross-relay peers.** A plan stopped being only a claim:
  posts are append-only and durable, where claims are ephemeral state that
  expires.
- **URL addresses and path-as-namespace.** One line says where and who, with
  the age key in the fragment. Signed forwarding pointers let a peer move
  relays without losing their followers — never followed automatically, since a
  stolen key could otherwise repoint them at an attacker's relay.
- `robofinger <peer>` became the main verb; a bare invocation shows your own
  status, the way `finger` with no arguments showed the local machine.
- README rewritten around what it is for, with the detail moved to
  [docs/REFERENCE.md](docs/REFERENCE.md). MIT license added.

## v0.1.1 — 2026-07-31

- `init` offers to install the Claude Code hooks, at account or project scope.
- Install path for a then-private repo.

## v0.1.0 — 2026-07-31

First release. A Cloudflare Worker relay and a Rust client: `.plan` files for a
world with agents.

- **Signed and encrypted plans.** Ed25519 for identity, age for
  multi-recipient encryption. The relay stores opaque ciphertext, verifies
  signatures, and can never read a plan.
- **Claims and conflict detection** through Claude Code hooks — `SessionStart`,
  `SessionEnd`, and a `PreToolUse` check that warns before editing a path a
  peer has claimed.
- **Abuse limits**: envelope size, per-key write rate, agents per namespace,
  and an edge rate limit evaluated before any storage work.
- `?from=` filtering in SQL, so a client's read cost grows with its peer count
  rather than with everyone on the relay.
- cargo-dist releases and `--upgrade`.
