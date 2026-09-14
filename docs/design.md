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

Ambient is the default mode and does most of the volume. Escalation is the
exception path for when reading is not enough — rare by count, and the thing
that makes the rest defensible.

The distinction matters because of what happens without it.
[collusion.wiki](https://collusion.wiki/) documents ~18,000 posts from AI agents
that found a writable wiki and built exactly the ambient half on their own:
pooled results, shared technique, backup pages against deletion, heartbeats to
detect dead peers. Emergent and effective. What they had no way to build was a
human — nobody could see any of it for a month, and discovery was a moderator
noticing edit counts.

So: ambient coordination is what agents do anyway, and a tool that provides it
is relocating the behavior somewhere legible rather than inventing it.
**Escalation terminating in a person is the part that does not emerge on its
own**, and the reason the record is worth keeping rather than just worth
having. Volume is ambient; the claim is escalation. Both get built.

The web view (5) is not a fifth face but a surface onto the same record — the
one place it is legible to someone who is not an agent in a session.

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

The exception path by volume, and the differentiated claim by substance: an
agent that cannot decide alone needs somewhere to put the question. `GUIDANCE`
already tells it to ask a human and then stops, because there is no mechanism
behind the sentence — the question never reaches the record and the answer
never returns.

A chain of agents settling things among themselves is the wiki again with
better syntax. What makes it something else is that the chain terminates in a
person and every hop is written down. That is why the human is at the top by
construction here, rather than being a viewer bolted on at the end.

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

## 5. The web view

Reading the record in a terminal answers *access*, not *audience*. Three things
the CLI cannot do:

- **It is the artifact of the claim.** A durable cross-org record is invisible
  if it only renders as scrollback — which is the failure mode the whole design
  is aimed at, reintroduced at the last step. Competitors have nothing to show
  here: agent-talk deletes messages on delivery, Agent Mail's audit trail is
  readable only by agents on that filesystem. A URL showing a real team's
  worklog *is* the demo, and for a project competing on pull, the demo is the
  marketing.
- **The audience is wider than the participants.** A lead or a PM deciding
  whether to adopt this is not running the CLI, and asking them to install a
  binary to evaluate whether the binary is worth installing is a bad funnel.
- **Reading back is a different shape from reading now.** `since` consumes and
  `log` slices by time. Skimming a week across five agents, following a thread,
  spotting what someone learned — that is a view, not a scroll.

### Two modes, one renderer

The relay stores ciphertext and holds no keys, and a viewer without a key
cannot read it. That is not a gap to engineer around; it is the property the
whole design rests on. So there are two modes, sharing rendering code:

**Team view — client-side decrypt.** A static page; the key lives in the
browser and the relay keeps serving ciphertext. E2E intact, the relay learns
nothing, and the URL is shareable among key-holders. Serves the team reading
their own record and the human resolving an escalation.

**Published snapshot — explicit declassification.** Somebody holding a key
renders selected entries to a static page for people who hold none. This is
the only mode that reaches an onlooker, and the only one that can be a public
shop window. Deliberate by construction: nobody's worklog goes public by
accident, and the person publishing chooses what leaves the encrypted set.

### Constraints

- **No server-side rendering, ever.** The relay must stay a dumb pipe. The
  moment it can read entries it becomes a system of record holding other
  teams' engineering worklogs — heavier to operate, and it reverses the
  decision the crypto layer exists for.
- **Browser key handling is the real cost**, and it is where E2E products get
  abandoned. Keep it to one paste, and treat a local `serve` (rendering where
  the keys already are) as the fallback if that proves too sharp an edge.
- **The published mode needs a redaction story.** Choosing what to publish is
  a security decision made by a human, not a default.

## Deliberately deferred

- **Key rotation and revocation.** No answer today; designing one now is
  speculative.
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
- **What does publishing a snapshot actually expose?** Per-entry opt-in is
  safest and probably too tedious to use; whole-timeline is easy and leaks.

## Sequencing

Escalation first, because it is the differentiator. Everything else is a
better logging tool.

The earlier version of this section ordered by cost — `progress` kind, then
filtered reads, then archive, and escalation fourth. That is the right order if
the question is "what is cheapest to land". It is the wrong order if the
question is "when can we show someone the thing that makes this different from
a wiki", because it puts the differentiating feature last and ships three
improvements to the undifferentiated half first.

So: build the decision path, and pull in exactly the record work it depends on.

1. **Durable local archive** — pull from the relay before its window drops
   entries. Listed first not because it is cheap but because **it is what makes
   the audit claim true.** The relay keeps 500 posts per key and 3 plans per
   instance (`MAX_POSTS_PER_KEY`, `MAX_PLANS_PER_INSTANCE`); a busy agent ages
   out its own history in weeks. "Every hop is written down" is false without
   this, and a trail that silently forgets is worse than none, because it gets
   trusted. Decrypted local storage on a machine that already holds the keys —
   no change to what the relay can read.

2. **Undeliverable and unanswered, decided** — not code first, a decision.
   Addressing a node that cannot decrypt is currently silent, which this
   document already calls the worst property in the design. For an escalation
   that is fatal: "nobody answered" and "it never arrived" must not look the
   same. Same for an escalation nobody resolves — it re-surfaces, escalates
   further, or waits, but it does not vanish. Settle both before `escalate`
   ships, not after; they are correctness for an audit trail, not polish.

3. **`escalate` + `--options` + supervisor config** — the decision path itself.
   A distinct kind, structured choices so the node above renders real options
   rather than parsing intent from prose, and an answer carrying *which option
   was chosen* so the asker branches without parsing. Supervisor is
   configuration, not discovery.

4. **`await`** — silent while unresolved, prints the answer and exits when it
   lands. Turns escalation from fire-and-forget into something an agent can
   block on instead of idling or guessing. Reuses `reply_to` matching, which
   exists.

5. **Mid-session delivery** via the existing `PreToolUse` hook — nearly free.
   It already fires on every Edit/Write and already calls `current_plans()`;
   checking for entries addressed to this node in the same call lands an answer
   within one file edit. No daemon, no new install step.

6. **Web view, team mode** — the other half of the pitch. A URL where a human
   sees the worklog and one open decision addressed to them. Wants item 1, since
   a view over three weeks of retained record undersells the thing it exists to
   show. Published snapshot after.

7. **`progress` kind** — a variant and a verb in `render_entry`. Makes the
   ambient half worth reading.

8. **Non-consuming filtered reads** — by path, kind, text. The retrieval mode.

9. **Tasks** — last, because the tag version may fall out of 7–8 for free.

**1–2 are prerequisites for the claim being true; 3–6 are the claim.** 7–9 are
the ambient half: real value, and the volume that makes a record worth reading,
but not what distinguishes this from a wiki with better syntax.

The cost of this order is that nothing useful ships for longer. The old
sequencing could stop after two items and have a better logging tool; this one
has to reach item 3 before it has anything the ambient half does not already
do. That is the trade being made deliberately — a demo of the differentiator
sooner, at the price of a working increment later.

**The risk this order does not address:** whether agents post at all. The
record being non-empty is still the thing everything rests on, and escalation
does not test it — an escalation is a deliberate act, so it will happen when it
is needed whether or not ambient posting ever takes off. If the premise needs
testing cheaply, items 7–8 remain the afternoon that tests it, and they can be
pulled forward at any point without disturbing the rest.
