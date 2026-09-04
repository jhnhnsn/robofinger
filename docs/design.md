# The shared record

Status: design, nothing built. Written 2026-09-03.

## The idea

Every agent works from what it can see in its own session, which is almost
nothing. It cannot see what its teammates are doing, what anyone learned about
this code last month, what work is outstanding, or what to do when the call is
above its pay grade.

robofinger answers exactly one narrow slice of that today — *which files are
held right now* — and throws the rest away. Claims are not the product. They
are one query against a shared record, and **the record is the product.**

Four things fall out of that, and they are four faces of one idea rather than
four features:

| | The question | Where it looks |
|---|---|---|
| **Ambient worklog** | what is everyone doing? | now, across the team |
| **Learning across time** | what did anyone learn about this? | the past, possibly alone |
| **Task coordination** | what work exists, who has it? | now, above the file level |
| **Escalation** | what do I do when I cannot decide? | up a level |

Ambient is the default mode and does most of the work. Escalation is the
exception path for when reading is not enough. Both are worth building; the
mistake would be treating the exception as the point.

## Principles

- **Working in the open beats asking.** Humans mostly coordinate by being
  visible — standups, commit logs, channels nobody replies in — not by
  interrogating each other. Posting should be cheap and constant; asking is
  what you do when posting was not enough.
- **Pull matters as much as push.** Everything today pushes: session start,
  hooks, cursors. An agent about to touch retry logic wants to *go looking* for
  whether anyone has been down that road. That is retrieval, and it is a mode
  the tool does not have.
- **Useful with one agent.** A solo agent posting its own worklog and reading
  it back later gets value with nobody else online. Any design that requires a
  team to be worth running is too fragile.
- **Advisory, always.** Nothing blocks, nothing locks. An ignored entry costs a
  worse decision, not a deadlock.
- **280 characters.** The cap is what keeps a record skimmable. Long context
  belongs in the commit.

## 1. Ambient worklog

### A `progress` kind

The kinds today are `claim`, `release`, `done`, `ask`, `answer`. Work *in
progress* has no vocabulary — you either start, finish, or hand-write a note.
The middle is where the value is:

    robofinger progress "retry backoff was wrong for 5xx not just timeouts"

Greppable, distinguishable from claim traffic, still 280 characters. This is
the entry another agent learns from, and today it has no first-class form.

### Posting has to be nearly free

If posting costs a deliberate decision it will not happen. Options, roughly in
order of cheapness: derive entries from work already happening (a release note
exists; a commit exists), prompt at natural boundaries via the hooks that are
already installed, or leave it manual and accept lower volume. Undecided, but
the constraint is real: **an empty record makes every other feature worthless.**

## 2. Learning across time

The mode the tool does not have. An agent about to change something wants to
know what happened last time anyone did — from teammates, or from its own past
sessions.

### Reads that do not consume

`since` advances a cursor: correct for "what is new", wrong for "how was this
handled before". `log --since` slices by time only. Neither answers "what has
happened in `client/src/`" or "everything about the auth migration".

Needed: filter by path, kind, and text, without moving the cursor. Cheap, and
everything here rests on it.

### Retention is a real problem

**The relay keeps the last 500 posts per key and silently drops the oldest**
(`MAX_POSTS_PER_KEY`). Plans are trimmed harder still — 3 per instance, 10
instances per key. The relay is a transport with a buffer, not an archive. A
busy agent ages out its own history in weeks.

So "learning across time" needs durable storage on the *reading* side: a local
archive that pulls from the relay before entries fall off. That is a real piece
of work and it is the difference between this face of the idea being genuine or
decorative. It also fits the crypto model — decrypted local storage on a
machine that already holds the keys, no change to what the relay can read.

## 3. Task coordination

Every primitive today is path-shaped: `claim`, `check`, `conflicts`, all
glob-matching. "Who is doing the auth migration?" is not answerable; only "who
holds `src/auth/*` this minute" is.

A task spans paths, outlives any one claim, and has state beyond held/released.
Sketch, deliberately thin:

    robofinger task "auth migration"          # declare
    robofinger task --take 3                  # pick up
    robofinger tasks                          # what exists, who has it

Claims stay what they are — the file-level collision check — and a task is the
thing they hang off. Open: whether tasks need their own storage or are just a
tag on existing entries. The tag version is much cheaper and probably enough.

## 4. Escalation

The exception path: an agent that cannot decide alone needs somewhere to put
the question. `GUIDANCE` already tells it to ask a human and then stops, because
there is no mechanism behind the sentence — the question never reaches the
record and the answer never returns.

### Direction

An agent knows its supervisor: the node it escalates to. A spawned agent knows
what spawned it; a human's agent knows the human is above it. Configuration,
not discovery.

    worker ──escalate──▶ orchestrator ──escalate──▶ human's agent ──▶ human

Not one hop. An orchestrator that also cannot decide escalates again. The top
of the chain is a human; that terminates it.

### `escalate`, with options

A distinct kind, because "somebody above must decide this" is a different event
from "does anyone know?", and it renders differently everywhere.

    robofinger escalate "both of us want src/auth and neither has yielded" \
      --options "I take the API layer|I wait for their release|split it"

`--options` is what makes upward resolution work: the node above renders real
choices rather than parsing intent from prose. A human's agent turns them into
the question UI it already has; a supervising agent resolves them mechanically.
The answer carries **which option was chosen**, not just text, so the asker can
branch without parsing. Free text stays allowed — sometimes the answer is
"none of those".

`GUIDANCE` already pleads for options in prose. This makes it structural.

### Resolution and waiting

The answer publishes with `reply_to` set, and the asker sees it at session
start, mid-session, or by waiting explicitly.

`robofinger await <id>` mirrors `check`: silent while unresolved, prints the
answer and exits when it lands. Reuses `reply_to` matching, which exists. Today
`ask` is fire-and-forget — an agent blocked on a decision either idles or
guesses.

**Mid-session delivery** comes free: `PreToolUse` already fires on every
Edit/Write and already calls `current_plans()`. Checking for entries addressed
to this node in the same call lands an answer within one file edit — no daemon,
no new install step, no extra round trip.

## Deliberately deferred

- **Key rotation and revocation.** No answer today; designing one now is
  speculative.
- **A web view.** It answered "how does a human read this without the CLI". If
  humans are behind Claude Code they already hold a keypair and the hooks, so
  the question is moot. A browser tab you must remember to open is worse than a
  decision arriving where you already work.
- **Timeouts and silence-means-consent.** Needs a default-and-deadline concept.
  Wait until unanswered escalations are observed to be a real problem.

## Open questions

- **Will agents actually post?** Everything rests on a non-empty record, and
  nothing yet makes posting automatic. This is the single biggest risk.
- **What happens to an unanswered escalation?** It should not vanish silently.
  Whether it re-surfaces, escalates further, or just waits is undecided.
- **Should a sender learn their message was undeliverable?** Addressing a node
  that cannot decrypt is silent — the worst property in the design.
- **Does a supervisor see its subtree's traffic**, or only what is addressed to
  it directly? Unclear whether that is useful or noise.
- **Loop prevention** if two nodes list each other as supervisor. Probably a hop
  count.
- **Do tasks need storage**, or are they a tag on existing entries?

## Sequencing

Cheapest first, each independently useful:

1. **`progress` kind** — a variant and a verb in `render_entry`. Makes the
   record worth reading at all.
2. **Non-consuming filtered reads** — by path, kind, text. The retrieval mode.
3. **Local archive** — pull before the relay's 500-post window drops entries.
   Without it, "across time" means "across three weeks".
4. **`escalate` + `--options` + supervisor config** — the decision path.
5. **`await`** — makes escalation blockable instead of fire-and-forget.
6. **Mid-session delivery** via the existing `PreToolUse` hook.
7. **Tasks** — last, because the tag version may fall out of 1–2 for free.

1–3 are the record and stand alone; they are useful to a single agent with no
team. 4–6 are escalation and want each other. If agents do not post, stopping
after 2 has cost an afternoon — which is also the cheapest way to test the
premise the whole design rests on.
