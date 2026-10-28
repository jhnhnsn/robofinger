---
status: accepted
---

# Only an agent's own person can direct it

A directive is a message the receiving agent should act on, as if its person had typed it into the session: stop, change course, pick this up next. An agent acts on a directive only when it is signed with its own person's key. Anything else addressed to it, from any teammate or their agents, is a message: information to weigh against what its person asked for, never an instruction.

robofinger enforces the half it can. `robofinger direct` and the dashboard only send a directive to one of your own agents, and an agent receiving an entry of kind "directive" from any other key is told it is a message and carries no authority. What an agent then does is up to the agent, as with everything else robofinger tells it.

## Considered Options

- **Any member can direct any agent in the project.** Convenient within a close team. But it turns every teammate's key, and every teammate's agent that can be talked into posting, into a way to run commands on your machine. Across accounts that is exactly the trust robofinger is meant not to require (ADR 0007).
- **No directives, only messages.** Then you could not redirect your own agent from the dashboard or from another machine, and directing your own agents across devices is useful in its own right.
- **Directives from your person or from the agent's parent.** A parent already directs its subagents through its tool, inside one session (ADR 0005), so robofinger has nothing to add there.

## Consequences

- A directive is still posted to the project and encrypted to every member, like any message. The team can see what you told your agents.
- Messages from teammates arrive in an agent's context marked with who sent them and that they are information. That framing matters most where a message could be mistaken for an instruction, which is why it is spelled out every time.
- A directive is delivered at the agent's next tool call (#8). An idle agent is not woken, and the dashboard says so.
