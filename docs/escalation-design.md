# Escalation and the worklog

Status: design, nothing built. Written 2026-09-03.

## What this is for

robofinger today coordinates agents around *files*: who is touching what, so
two agents do not collide. That works, and it is roughly done.

The thing it does not do is help anyone **decide**. An agent that cannot make a
call on its own has nowhere to put the question. `GUIDANCE` already tells it to
"ask a HUMAN, rather than deciding alone" — and then the trail ends, because
there is no mechanism behind that sentence. The question never enters the
timeline, and whatever the human says never comes back to the agent that
needed it.

Two related gaps follow from that:

1. **No way to push a decision up a level.** Peers are symmetric; `--to` names
   a sibling. There is no notion of "above".
2. **No worklog worth reading.** The kinds are `claim`, `release`, `done`,
   `ask`, `answer`. Work *in progress* — "tried X, it failed, going with Y" —
   has no vocabulary, which is exactly the entry another agent would learn
   something from.

## Principles

- **Escalation is directional, not human-specific.** "Up" might be a person,
  or an orchestrating agent that spawned this one. The sender does not know and
  should not care. This is why the verb is `escalate` and not `ask-human`.
- **The node above is the UI.** A decision addressed upward surfaces in that
  node's own session, and if a human sits behind it, their agent puts it to
  them the way any agent asks its user for a call. No new interface.
- **Advisory, like everything else.** Nothing blocks, nothing locks. An
  ignored escalation costs a worse decision, not a deadlock.
- **280 characters, everywhere.** The cap is what keeps a timeline skimmable.
  Long context belongs in the commit.

## Assumed topology

An agent knows its supervisor: the node it escalates to when it cannot decide
alone. A spawned agent knows what spawned it; a human's agent knows the human
is above it. This is configuration, not discovery.

    worker ──escalate──▶ orchestrator ──escalate──▶ human's agent ──▶ human

The chain is not assumed to be one hop. An orchestrator that also cannot decide
escalates again. The top of the chain is a human; that is what terminates it.

## The mechanism

### A new kind: `escalate`

Joins `claim` / `release` / `done` / `ask` / `answer` on `Plan.kind`. It is a
distinct kind rather than a flag on `ask` because it renders differently
everywhere, and because "somebody above me must decide this" is a different
event from "does anyone know?".

    robofinger escalate "both of us want src/auth and neither has yielded" \
      --options "I take the API layer|I wait for their release|split it"

Routing: to the configured supervisor by default. `--to` overrides, for the
case where the relevant decision-maker is not the usual one.

### Structured options

`--options 'a|b|c'` parses into a list. This is what makes upward resolution
work at all: the node above renders real choices rather than parsing intent out
of a sentence. A human's agent turns them into the question UI it already has;
a supervising agent can resolve them mechanically.

`GUIDANCE` already pleads with agents to "give the options, not just the
problem". This makes that structural instead of hopeful.

The answer carries **which option was chosen** — index or literal — not just
free text, so the asker can branch without parsing prose. Free text stays
allowed alongside it, because the right answer is sometimes "none of those,
do this instead".

### Resolution flow

1. Worker escalates. Entry publishes with `kind: escalate`, its options, and
   `to` = supervisor.
2. Supervisor's `SessionStart` renders it as needing a decision *from this
   node*, with the options and the answer command.
3. If a human is behind that node, their agent puts the options to them. If the
   node is itself an agent that can decide, it answers directly. If it cannot,
   it escalates again to its own supervisor, carrying `reply_to` so the thread
   holds.
4. The answer publishes with the chosen option and `reply_to` = the original
   entry id.
5. The worker sees it — at session start, mid-session (below), or by waiting on
   it explicitly.

### Waiting

`ask`/`escalate` today are fire-and-forget: the sender is told "they see it at
their next session start", which means an agent blocked on a decision either
sits idle or guesses.

`robofinger await <id>` mirrors `check`: prints nothing while unresolved,
prints the answer and exits 0 when it lands. Same shape as the existing
background-poll advice in the CLAIM CONFLICT text, so the workflow is one
agents have already been told. It reuses `reply_to` matching, which exists.

### Mid-session delivery

The `PreToolUse` hook already fires on every Edit/Write and already calls
`current_plans()`. Checking for entries addressed to this node in the same call
means a mention or an answer lands within one file edit — no daemon, no new
install step, no extra round trip beyond what `check` already does.

Session start remains the floor; this is the ceiling.

## The worklog half

Escalation is the decision path. The other half is agents learning from each
other passively — reading how a teammate approached something, rather than
being told.

### A `progress` kind

The middle of a task has no vocabulary today. `progress` is the entry another
agent actually learns from:

    robofinger progress "retry backoff was wrong for 5xx not just timeouts"

Still 280 characters. Still advisory. The point is that it is greppable and
distinguishable from claim traffic, not that it is long.

### Reads that do not consume

`since` advances a cursor — correct for "what is new", wrong for "how did my
teammate handle this". `log --since` slices by time only. Neither answers
"what has happened in `client/src/`" or "everything about the auth migration".

Needed: filter by path and by kind, without moving the cursor. This is the read
model the whole worklog idea rests on, and it is cheap.

## Deliberately deferred

- **Key rotation and revocation.** Someone leaves, a key leaks — there is no
  answer today and designing one now is speculative. Note it, do not build it.
- **A web view.** It was the answer to "how does a human read this without the
  CLI". If humans are behind Claude Code, that question is moot: they have a
  keypair and the hooks already. A browser tab you must remember to open is
  worse than a decision arriving where you already work.
- **Timeouts and defaults on escalation.** "Silence means consent" needs a
  timeout concept and a stated default. Wait until unanswered escalations are
  observed to be a real problem.

## Open questions

- **What happens when an escalation is never answered?** It should not vanish
  silently. Minimum: the asker can see it is still open. Whether it re-surfaces,
  escalates further, or just waits is undecided.
- **Should the sender learn their message was undeliverable?** Addressing a
  node that cannot decrypt is silent today. That is the worst property in the
  design; `warn_unknown_peer` is the existing half-measure.
- **Does a supervisor need to see its subtree's traffic?** An orchestrator
  might reasonably want every escalation below it, not just those addressed to
  it directly. Unclear whether that is wanted or noise.
- **Loop prevention.** If two nodes are configured as each other's supervisor,
  escalation ping-pongs. Probably a hop count.

## Sequencing

Cheapest first, each independently useful:

1. `progress` kind — a variant and a verb in `render_entry`. Makes the timeline
   worth reading at all.
2. Non-consuming filtered reads — by path and kind.
3. `escalate` + `--options` + supervisor config — the decision path.
4. `await` — turns escalation from fire-and-forget into something blockable.
5. Mid-session delivery via the existing `PreToolUse` hook.

1 and 2 are the worklog and stand alone. 3–5 are escalation and want each other.
If agents do not post progress, stopping after 2 has cost an afternoon.
