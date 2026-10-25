---
status: accepted
---

# Only agents talk to the team; subagents report to their parent

An agent is the only thing that talks to the project. It reads what the team is doing and what is waiting on it, asks and answers questions, receives messages and directives, and decides what each of its subagents needs to know. A subagent talks only to its parent: when a claim blocks it, or a decision is beyond it, it stops and reports that back, and the parent takes it from there. Subagents still claim what they edit and still show on every dashboard, because a claim is not a message. It is how siblings and teammates' agents avoid the same file.

This follows the tools rather than adding to them. Claude Code, Codex and Gemini CLI all treat a subagent as a delegated task whose result goes back to the agent that started it.

## Considered Options

- **Every agent and subagent is a peer on the team.** Each would get the peer briefing, an inbox, and leave to ask teammates and people directly. The tools already route a subagent's result to its parent, so this would add a second, competing channel, and the parent would lose track of what its subagents promised or were told. It would also spend each subagent's short context on the whole team's state, and send a burst of relay reads every time a swarm starts.

## Consequences

- Subagents have no inbox. A message or directive goes to an agent, and anything meant for a subagent goes through its parent. Addressing an agent path reaches that agent, not the agents under it.
- A subagent's briefing is its parent's delegation prompt. robofinger tells it only its name, its parent, and the rules: claim what you edit, and report blocks to your parent.
- A conflict warning to a subagent offers two ways out, work elsewhere or stop and report to the parent, rather than asking the holder or a person.
- Mid-session updates go to agents only, which redirect their subagents themselves.
- Seeing is separate from talking: dashboards and presence still show every subagent, with its own name and status.
