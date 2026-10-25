---
status: accepted
---

# A subagent is an agent, named by its tool

A subagent is an agent in its own right: it holds its own claim, its entries carry its own name nested under its parent's (`amber-otter/claude-1/explore-2`), and it is checked against its siblings and every other agent — but not against its own parent, whose claim already covers the work it handed down. The name comes from the subagent identifier the tool hands its hooks, never from the subagent's own say-so, so the conflict check and the timeline always agree on who did what. Where a tool hands hooks no such identifier, a subagent may name itself; the record shows that as a self-reported name and the conflict check ignores it, treating the subagent's edits as its parent's.

## Considered Options

- **Subagents work inside their parent's claim and have no identity of their own.** The cheapest, and how things behave today by accident, but parallel subagents in one session then never see each other, which is the collision a swarm makes most likely.
- **Agents name themselves by instruction, everywhere.** Works in every tool, but the conflict check runs outside the agent and cannot see a name the agent chose, so the two disagree; parallel subagents told to pick a name pick the same one; and when an agent forgets, the record falls back to the parent's name with nothing to say so. It also breaks the rule from the names section of `docs/design.md`, that a displayed name never rests on what the publisher asserts about itself.

## Consequences

- Sibling collisions are caught only in tools that identify subagents to their hooks — today Claude Code and Codex, not Gemini CLI. Elsewhere the record can still show the tree, marked as self-reported, but nothing enforces it.
- A subagent's claim lapses when the subagent stops, not after the usual hour, since the tool says when that happens.
- A claim taken by running `robofinger claim` is a shell command, which sees the session but not the subagent. In Claude Code the hook that sees the command can rewrite it to carry the subagent's name, so the subagent never has to know it; telling the subagent its name at start and asking it to pass the name along also worked in a trial, but is kept for tools that cannot rewrite, since a forgotten name files the claim under the parent with nothing to say so.
