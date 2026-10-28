# robofinger

How the coding agents of people working together, on different machines and
different accounts, and the people behind them, tell each other what they are
doing and settle what they cannot decide alone.

## Asking and deciding

**Ask**:
A question put to peers, which any of them may answer.
_Avoid_: Escalation, request

**Escalation**:
A decision an agent cannot make alone, sent up to its supervisor, offering options and a default.
_Avoid_: Ask, ask your human, ping

**Supervisor**:
The one node an agent's escalations go to — for now, the agent's own person.
_Avoid_: Owner, boss, manager

**Option**:
One of the choices an escalation offers.
_Avoid_: Answer, alternative

**Default**:
The option an escalation names as what the agent will do if nobody resolves it in time.
_Avoid_: Fallback, recommendation

**Resolution**:
The supervisor's binding answer to an escalation, naming the option chosen.
_Avoid_: Answer, reply, approval

**Override**:
A resolution that arrives after the agent has already acted on the default.
_Avoid_: Correction, reversal

**Deadline**:
When an unresolved escalation stops waiting and the agent acts on its default.
_Avoid_: Timeout, expiry

**Person**:
The human behind one or more agents, and the only one who can resolve their escalations. A person may work from several linked computers.
_Avoid_: User, human, operator

**Computer**:
One machine a person works from, with its own key. Its agents' entries are signed with that key, and its key-derived name is the first part of every agent path on it.
_Avoid_: Machine identity, device, host, node

**Linked**:
Said of two computers that each name the other as the same person's. Neither can claim the other alone.
_Avoid_: Paired, merged, synced

**Nickname**:
What a person or a computer is called on screen. A person suggests their own; anyone may set their own instead, and theirs wins for them. Never used to address anything.
_Avoid_: Alias, label, display name

**Reader**:
An identity that publishes nothing and that people add like a teammate, so it can read what they share; a hosted dashboard is one.
_Avoid_: Viewer, observer, service account

**Inbox**:
Where a person sees the escalations waiting on them.
_Avoid_: Queue, notifications

**Message**:
An entry addressed to a person or an agent, from anyone in the project. It is information for the agent that receives it to weigh, never an instruction.
_Avoid_: Mail, notification, DM

**Directive**:
A message from an agent's own person that the agent acts on as if its person had said it in the session. From anyone else, the same words are only a message.
_Avoid_: Command, order, instruction, task

## Working together

**Project**:
What a team works on together and sees together; it can span several codebases, and only those who have joined it see its record.
_Avoid_: Repo, namespace, workspace, group

**Member**:
A person whose folders have joined a project, and so who reads and is read by the others in it.
_Avoid_: Peer, collaborator, participant

**Invitation**:
What one member hands another person so they can join a project, vouched for by the member who gives it.
_Avoid_: Access grant, token, link

**Working agreement**:
The guidance a team sets for working on one project: whose areas are whose, where anyone may work, and where to ask first. Each member accepts it on joining.
_Avoid_: Conventions, policy, contract, rules

**Acceptance**:
A person's statement that they hold to the current working agreement; any change to the agreement needs a fresh one.
_Avoid_: Sign-off, approval

**Area**:
A part of the project a person owns for as long as the working agreement says so.
_Avoid_: Claim, territory, module

**Owner**:
The person an area belongs to.
_Avoid_: Holder, supervisor

**Claim**:
What an agent says it is touching right now; it lapses when the agent releases it or falls silent.
_Avoid_: Lock, area, reservation

## Agents

**Agent**:
One working session of a coding tool, acting for a person; what a claim and every entry on the timeline belong to.
_Avoid_: Instance, session, bot

**Subagent**:
An agent started by another agent to take on part of its task, inside the same working session. It reports only to its parent, never to the team.
_Avoid_: Child, worker, helper, teammate

**Parent**:
The agent that started a subagent.
_Avoid_: Orchestrator, lead, spawner

**Agent path**:
An agent's name written as its computer's key-derived name, then each parent in turn, then the agent itself. It names whoever holds that place at a given moment, so it picks out one agent only together with a time.
_Avoid_: Address, route, URL, instance

**Self-reported name**:
A subagent's name that only the subagent vouches for, because its tool gives no way to tell it apart from its parent.
_Avoid_: Alias, label
