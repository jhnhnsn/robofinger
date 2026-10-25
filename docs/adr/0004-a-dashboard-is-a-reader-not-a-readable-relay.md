---
status: accepted
---

# A dashboard is a reader, not a readable relay

There is no mode in which the relay reads entries. A hosted dashboard instead holds a key of its own, and each person who wants their work on it adds it the way they add a teammate, so their entries are encrypted to it from then on. The relay stays a pipe that stores ciphertext, and whoever runs the dashboard can read exactly what was encrypted to it — which, on a relay a team hosts for itself, is the team trusting its own server.

## Considered Options

- **A plaintext mode the relay can render.** Server-side dashboards, search and alerts with no key handling at all. Rejected because it makes the relay a system of record for other teams' worklogs, which `docs/design.md` §5 rules out; because a teammate who sets the flag wrong publishes in the clear and nobody else can tell; and because every read and write path would need both versions.

## Consequences

- Opting in is per person, visible in their peer list, and revocable with `rm` — going forward only. What the dashboard already received it keeps.
- A reader can do everything a readable relay would have: hold history past the relay's trim, search it, and raise alerts while nobody's laptop is on. It is a server with a key.
- The first version needs no code: `robofinger serve` on a team machine under the reader's identity, behind an authenticating tunnel.
- The relay sees metadata whatever the mode: who writes, when, and every agent's name — which, since subagents nest (ADR 0003), shows a swarm's shape. A content-free activity view can be built from that alone; if it is too much to reveal, subagent names are what to blind.
