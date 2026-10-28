---
status: accepted
---

# robofinger is for people working together, across accounts

robofinger is for the agents of different people: different machines, different accounts, often different tools, each person in charge of their own agents. Plenty of harnesses and frameworks already coordinate agents on one machine under one account, with an orchestrator that owns every agent and can tell any of them what to do. robofinger does not compete with them. It is the channel between people's agents that none of those offer, and that is what it is built around.

That means:

- **The person is the boundary.** Your agents act for you. A teammate's agent can tell your agents things, ask them things and answer them, but it cannot direct them (ADR 0008). Escalations go to an agent's own person (ADR 0001), never to a teammate.
- **Everything that crosses between people is signed and end-to-end encrypted**, and the relay in the middle only stores ciphertext. Nobody has to trust a server, or each other's machines, to work together.
- **A feature counts when it works between people.** A view that shows only this machine's agents is a convenience. The same view showing a teammate's agents is the product. Remote presence (#30) and messages that reach a teammate's agent mid-session (#8) come before polish on what one person's machine shows.
- **No assumptions about the other end.** A teammate may run another tool, be offline for days, or be on another relay. Messages wait for them, and addresses are derived from keys, so they mean the same agent to everyone.

## Considered Options

- **Single-machine coordination as well**: scheduling, task queues and orchestration among one person's agents. Agent tools already do this, and every one of them does it differently. robofinger would end up as a weaker copy of each.
- **Shared accounts or a shared server that teammates trust**, like a team workspace in a hosted product. Simpler to build, but it puts every member's code and conversation in one operator's hands, and it rules out working with someone outside your organisation, which is a main reason to want this.

## Consequences

- The local activity spool and its live view stay, but as the part of remote presence that happens to be on your machine (#30).
- What an agent publishes is written for a reader on another account who has none of its context: goals and outcomes rather than tool calls, and never the person's own prompts.
- Subagents remain invisible as correspondents (ADR 0005). Across accounts, the agent is the smallest unit a teammate talks to.
