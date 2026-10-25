---
status: accepted
---

# A folder joins a project by invitation, and the working agreement is guidance in the repo

A folder publishes nothing until it joins a project. To join, someone already in the project hands over an invitation: one string naming the relay and the project, listing the members, and signed with the inviter's key. `robofinger join <invitation>` in a folder writes that folder's project settings, adds the members as peers for the project, and posts a signed "joined" entry, so the members add the newcomer on the strength of a member's signature. A project can be joined from any number of folders, such as a client repo, a docs repo or a worktree. Within a project, claims still conflict only inside one codebase, so two repos' `src/**` never collide.

Entries stay encrypted to each member, as today. The invitation only fills in who to encrypt to. Removing a member means no longer encrypting to them, from then on.

The working agreement is a file in the repo, `AGREEMENT.md`, saying whose areas are whose, where anyone may work, and where to ask first. It is guidance for agents and their people, not enforcement. Agents are told it at session start, conflict warnings name an area's owner, and the dashboard shows it. Joining records which version of the agreement the member accepted, and a changed agreement asks members to accept again.

## Considered Options

- **One relay for everything a machine does**, as today. Every folder publishes, so unrelated work (another client's repo, a side project) lands on a team's dashboard. That is how this decision came up.
- **A shared project key in the invitation.** The simplest to build, but anyone who ever held the invitation reads everything, and removing one person means changing the key for everyone.
- **A project is exactly one repo.** Simpler, but a product split across repos becomes several projects that cannot see each other.
- **The agreement on the relay**, signed and distributed through the project. Members without repo access could read it, but it would sit outside code review, and the repo is where the areas it names live.
- **Enforced areas**, where edits outside your area are refused. Areas change faster than any rule file, and a tool that blocks work gets uninstalled. robofinger stays advisory, as its claims are.

## Consequences

- An unjoined folder still tells its agent its name and keeps its local activity spool. It just publishes nothing.
- The global config holds identity only: keys, and how to reach a relay. Which project a folder belongs to lives with the folder. Existing installs need a migration. The current global relay becomes the project that the folders already publishing to it join explicitly.
- Invitations are bearer secrets until they expire. An invitation should expire, and joining with one leaves a signed entry naming who invited whom.
- "Which codebase" (the conflict namespace) and "which project" (what is seen together) are separate settings, as #14 proposed with `name` and `group`.
