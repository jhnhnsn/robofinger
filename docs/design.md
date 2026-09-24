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
one place it is legible to someone who is not an agent in a session. Rooms
(6) are not a face either — they are the addressing layer all five rest on,
and the only part of the design that gets harder as a team grows. Names and
provenance (7) sit underneath both.

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
- **140 characters.** The cap is what keeps a record skimmable. Long context
  belongs in the commit. Tweet length rather than 280 because the record is
  read in an agent's context window, where every entry competes with the work
  it is trying to do — and because a cap loose enough to summarise invites a
  summary of what the diff already says.

## Three layers of scope

Namespace, group and project are discussed separately throughout this document
and are easy to confuse, because all three sound like "which entries do I see".
They answer different questions, and expecting one to do another's job is the
main way to get this wrong.

| | Question | Mechanism | Enforced by |
|---|---|---|---|
| **Path** (namespace) | where is it stored? | `WHERE ns=?` — exact match | the relay, as a partition |
| **Group** | who can read it? | encrypted to those peers' keys | cryptography |
| **Project** (repo) | is it relevant here? | `p.project == here` | the client, as a filter |

**Path scopes the corpus; group scopes the audience.** An entry lives at one
path and is readable by one set of keys, and neither constrains the other. So
they compose: a path per deployment or organizational unit, groups within it
for roles or teams. `/acme/cura` holds everything for that product; inside it
`--group leads` carries decisions the whole fleet should not see.

The rule that follows: **if the reason is "this is noise for them", that is a
path or a filter. If the reason is "they should not see this", it has to be a
group.** Splitting a path is free and hides nothing — anyone can query any
namespace, and anyone in your peer list decrypts what they find. Splitting a
group is the only thing that actually withholds content, and it costs a
decision per post.

### Paths nest in notation, not in behavior

`/acme/cura/mobile` reads as a hierarchy and is free to adopt — `ns` is an
opaque string, so nested paths work today with no change. But it is exactly
that: notation. Every query is exact-match, so `/acme` is not a parent of
`/acme/cura`, it is an unrelated sibling. Two agents one path segment apart are
as isolated as two on different relays.

That matters because escalation wants the opposite. A supervisor seeing its
subtree would need prefix queries in the relay, and more importantly an answer
to an open question below — whether a supervisor sees its subtree's traffic at
all. Until that is settled, a path hierarchy would *imply* containment the
system does not implement, which is worse than a flat name.

So the escalation chain stays what §4 says it is: configuration, not
discovery. A declared graph, deliberately not derived from addressing, because
an org's escalation path and its storage layout want to vary independently.

### What this gives the web view

Grouping in the view comes from the crypto rather than from a UI concept
invented for it: a viewer holds a key, fetches one namespace, and renders what
it can decrypt, with groups as the natural swimlanes. The boundary is already
real in the data.

The asymmetry to know is that groups work *within* what you can fetch. A group
spanning two namespaces means two stores and a client-side merge. Groups are
comfortably intra-path; paths are the coarser cut.

## 1. Ambient worklog

### A `progress` kind

The kinds today are `claim`, `release`, `done`, `ask`, `answer`. Work *in
progress* has no vocabulary — you either start, finish, or hand-write a note.
The middle is where the value is:

    robofinger progress "retry backoff was wrong for 5xx not just timeouts"

Greppable, distinguishable from claim traffic, still 140 characters. This is
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
thing they hang off. Whether tasks need storage of their own is settled in 6
and 8: they do not. A task with a tracker behind it is a ticket claim; one
without is a tag on entries in a room's namespace.

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

## 6. Rooms

The other five all assume an audience, and none of them defines one. Today the
audience is a local guess: peers may be tagged with groups, and `post --group
work` encrypts to whoever *you* tagged `work`. There is no shared object, which
costs two things.

**Onboarding is N².** A twelfth person joining a project needs eleven existing
members to run `add`, each picking their own label for them. Fine at three. Not
fine at twelve, and "a team and its agents on one record" is a twelve-person
shape.

**A thread's audience drifts between hops.** Alice posts to her seven. Bob
replies with `reply_to` set — to *his* nine. The exchange is now readable by a
set nobody can enumerate, including the people in it. Claims dodge this by
ignoring groups on purpose; `ask` and `answer` dodge it by going to everyone.
Group posts do not, and `reply_to` is what exposes it.

### A room cannot be a destination

`relay/README.md` fixes the shape of any answer: storage is keyed by publisher
pubkey, writes are single-writer and monotonic, and the relay "decides nothing
about who may read whom". A room that members *write into* forces the relay to
check a roster before accepting a write, which is an access decision. That ends
the dumb pipe — so it is out on the contract, not on taste.

What is left is that the relay serves exactly one shape: a keypair publishes
signed state, peers fetch it. So a room should be a keypair too.

### A room is an identity that publishes a roster

    GET /u/<room-pubkey>   →   roster: alice·key, sam·key, dev-bot·key, …

Joining is `add` against a room address in the format that already exists, with
the same paste-it-in-Slack story and the same `--as` override. One fetch
replaces eleven adds: **onboarding goes from N² to N.** Entries still publish to
your own key, still single-writer, still monotonic. The room is a discovery and
audience object, not a store, and the relay contract does not change — which is
fitting, since it already calls a namespace a room.

### Roster public, content private

If the roster were encrypted to its members, a newcomer could not read it to
find out how to reach them. So publish it in the clear and keep every entry
encrypted to it: you learn who is in a room, never what they said. Admission is
then exactly one act — the owner adding a key is what makes the next entry
decryptable to it — and joining stays a pure read with no handshake.

### A directory is a room you have not joined

A public roster is an enumerable list of who is in a namespace, which is a
directory — there is no second object to build. What separates the two words is
curation, not structure:

- **Curated**, signed by the room key, and therefore safe to encrypt to. A room.
- **Self-asserted** — whoever happens to publish in the namespace — and therefore
  only safe to look someone up in. A directory.

The signature is what promotes a lookup list into an audience. Without it,
anyone who wrote themselves into the namespace would receive everyone's entries.

In the terms of **Three layers of scope**, a room is a path plus a *shared*
group: the group mechanism unchanged, with its membership published rather
than held privately by each publisher.

This also keeps a promise the README makes: "no directory, nobody's feed to be
ranked in". A roster is not a registry — per-namespace, opt-in, unranked — and it
grants nothing. Reading is pull-only from your own follow list, and following
someone does not let you read them, so finding a key creates no inbox and no way
to push anything at anyone. **A directory replaces address-swapping, not
consent.** It needs the one relay change rooms want anyway: enumerate a
namespace, rather than `?from=` a list of keys you already hold.

### Invites label a knock, they do not grant anything

A room has an age key like any identity, so someone outside the roster can still
encrypt *to the room*. That is a join request, readable only by the owner, and it
replaces passing an address out of band — at the cost of the first unsolicited
inbound channel in the design. Which is what an invite token is for:

    $ robofinger room acme --invite
    https://acme@relay.example.com/u/KEY?invite=7fq2mk#age1…
      one-time, expires in 7 days

    $ robofinger room acme
    pending:
      amber-otter   invite 7fq2mk (issued to bob, 2d ago)
      slate-heron   no invite

The token authorises nothing. There is no server to check it against, and
pre-authorising — "anyone holding this gets in" — would need something to sign a
roster while the owner is away, which means a daemon. It is a correlation hint
that makes a pending list triageable, enforced locally by the client that issued
it, and admission is still a human adding a key.

Two details: the token goes in a query parameter *before* the fragment, because
`#` already carries the age key; and the joining client strips it into the
encrypted request body rather than sending it to the relay, for the same reason
the age key sits after `#`.

Verification is a different job and is already done — the derived alias is a
pubkey fingerprint, so "does yours say amber-otter?" is an out-of-band check in
two words.

### Audience is the roster minus your blocks

The obvious rule, "encrypt to the roster", hands your recipient list to the
room's owner and destroys the best property in the security model: `rm bob` is
unilateral, cryptographic, instant, and needs no relay cooperation.

So a roster only ever *adds* recipients you never had to add by hand, and a
local block *subtracts*, from every room, always. Stated plainly, because a user
has to be able to hold it in their head: **joining a room delegates audience
curation to its owner; blocking is how you take it back.**

### What it costs

- **An owner.** The first asymmetry in a deliberately symmetric design. It stays
  honest because the room key is another keypair on somebody's laptop — no
  server role, no signup — and because `/forward/<pubkey>`, which exists for "I
  moved", is also how a room hands off to a successor.
- **Removing someone becomes policy, not crypto.** A key dropped from the roster
  stops receiving only once every member's client honours the new roster. Same
  honest-client assumption as claiming being best-effort, with a sharper edge:
  an ignored claim costs a worse decision, an ignored removal leaks. Document
  that, rather than letting people assume room removal is as strong as `rm`.
- **Membership becomes observable.** Publisher keys ride in cleartext, so a
  relay can already infer the graph from `?from=` batches. A public roster makes
  that trivial rather than inferential — a change of degree, but worth saying.

### The size ceiling

age wraps the file key once per recipient, on the order of 120 bytes of stanza
each. A claim republished on every edit carries roughly 4KB of recipient
overhead at 30 members and 24KB at 200, against a relay that bounds envelope
size and a 100–300ms hook budget.

So multi-recipient encryption is right for rooms of dozens and wrong for rooms
of hundreds. The alternative is a room-wide symmetric key, wrapped per member in
the roster object: constant entry size, and every removal becomes a key
rotation. Ship multi-recipient — it keeps `rm` instant and teams are small. The
room key is what a room adds when it outgrows that, not before.

### What it settles

- **Undeliverable addressing, inside a room.** A shared roster lets the client
  check that an addressee can decrypt *before* publishing, and say so when it
  cannot. Addressing someone outside every shared room is still silent, so the
  open question shrinks to that case rather than closing.
- **Tasks need no storage of their own.** A room is a namespace, and
  `example.com/plan/team-a` already means separate storage — so a task is a tag
  on entries in one, which is the cheap version from 3 with a scope that also
  keeps `?from=` bounded. Where a tracker exists, 8 is cheaper still.
- **The directory question.** There is nothing else to design: a roster is
  what a directory would have been.

## 7. Names and provenance

A name does three jobs — read as an address, read as a byline, and read as
credit — and they want different things. Splitting them is what lets a name
carry the model and the effort without becoming unstable.

### The address is what survives change

    amber-otter/claude-1

Both halves are machine-read, which is why they are structured at all. The
first is derived from the pubkey rather than the hostname, because the alias
rides *outside* the encryption and `johns-macbook-pro` is a needless broadcast.

**The reader derives it, and that is the whole point.** A name taken from the
envelope is asserted by the publisher, so a peer can appear as anyone by
renaming itself between entries — on a timeline whose only job is saying who
did what. So a displayed name comes from one of the two places the publisher
does not control: the label you filed them under with `add --as`, or failing
that the name their key derives to. The published alias keeps its one job,
suggesting a label at `add` time, which is what the address format has always
called it.

That makes the name checkable out loud — "does yours say hazel-hare?" — and it
means a rename applies backwards through the record, because the name is
computed from an identity rather than stored on an event. Which is the line
between the two halves of this section: the byline below records what was true
when an entry was written, the name records who the key is, and only one of
those can be restated later.

The second half of the address is the session slot: `PreToolUse` has to resolve to the same agent
that took the claim, and the relay stores 3 plans per instance, so two sessions
that collapse into one name overwrite each other's claims.

Model and effort therefore cannot be path segments. Two Opus sessions would
share a slot, and `--to amber-otter/claude-opus-5/high` names nobody the moment
a rate limit downgrades them mid-task. **Anything before a slash is an address,
and an address has to survive the thing it names changing.**

### The byline is a fact about a fixed past event

    14:02  amber-otter/claude-opus-5/high (alice)   progress: backoff was wrong for 5xx

An entry is immutable, so stamping what produced it at write time is safe in
exactly the way a live identity is not. Two fields, both `#[serde(default)]`
so older clients drop them: the model string as the agent reports it, and the
configured effort.

**Effort is the configured setting, not a self-assessment.** low/medium/high is
a fact about how the run was set up; "how hard I found this" is an agent marking
its own homework. It is also what justifies the third segment — the same model
at low and at high are different producers, and the model name alone hides it.

Both are self-asserted, since the client cannot introspect its own model. That
is tolerable for a capability hint and would be disqualifying for a name: a
false effort tag is noise, a false identity is impersonation.

### The nickname is yours and never leaves the machine

`(alice)` is the local label from `add --as`, rendered in and not published.
The first segment is global and key-derived, the parenthetical is local and
yours — the petname split made visible, so an unmemorable derived name costs
nothing at reading time. Omitted when you have no label filed for that key.

### Two things fall out

**Origin, for free.** A human has no model and no effort, so a bare `sam`
against `sam/claude-opus-5/high` separates hand-typed from agent-emitted with
no extra field. That is the distinction that matters when reading back to work
out whether a person ever actually looked at something.

**Record it before anything queries it.** Nothing needs to read these fields for
them to be worth writing: effort is retroactively unrecoverable, so either it
was stamped on the entry or the question is permanently unanswerable. Stamp it,
build no view for it, and put "does anyone ever query effort?" in Open questions
rather than designing for an answer nobody has asked for yet. Store the model
string verbatim and render it short — `opus-5/high` in the log, the full string
on a detail view — because 34 characters of byline against a 140-character cap
is a skimmability problem, not a storage one. The byline says what produced an
entry; the `commits` field says what it produced.

## 8. Tickets, tags and channels

### A ticket claim is a claim with a different namespace

`project` exists so `src/**` in one repo cannot conflict with another. Point it
at a tracker instead of a repo and the whole claim machinery — expiry, the
timeline, release notes, `since`, conflict warnings — works on work items with
no new mechanism at all.

| | files | tickets |
|---|---|---|
| namespace | `project` = repo name | `project` = tracker host |
| identifier | glob | ticket id |
| match | glob match | case-insensitive exact |

The thing claimed stays a positional argument, because a glob, a ticket id and a
URL are cheaply distinguishable:

    robofinger claim "migrate session store" 'src/auth/**'
    robofinger claim "fix the retry backoff" PROJ-123
    robofinger claim "reviewing this" https://github.com/o/r/pull/41
    robofinger claim "…" --ticket 41      # when a bare id would read as a path

`--ticket` earns its place as the disambiguator rather than the usual path, and
it is the only place the noun surfaces — a URL names itself, so the word only has
to cover short ids. `ticket` over `item`, which says nothing; over `issue`, which
would collide with the tool's own warning language and implies a defect when half
of what people claim is a feature; and over `task`, which is already the
free-text field on every claim.

**Normalisation is the whole feature.** `PROJ-123`, `proj-123` and the full
browse URL have to collapse to one identity, or two agents hold the same ticket
and neither is warned — a warning that silently does not fire, which is the
failure this project exists to prevent. The client splits a pasted URL into host
and id and compares case-insensitively, as alias matching already does.

**Where a ticket lives reuses the mechanism that already exists.** v0.7.0 added
`commit_url_template()`: derive from `origin` where the host is known, accept
`ROBOFINGER_COMMIT_URL` as a `{sha}` template otherwise, and return nothing
rather than guess a path segment. Tickets want exactly that, with `{id}`:

    ROBOFINGER_TICKET_URL=https://acme.atlassian.net/browse/{id}

Derivable where a forge hosts its own issues — `/issues/{id}` on GitHub,
`/-/issues/{id}` on GitLab — and required otherwise, because Jira and Linear sit
on a host `origin` never mentions. The same refusal carries over: an
unrecognised host gets no link and a line saying how to supply one, since a
ticket link that 404s cannot be told apart from a ticket that was closed and
archived.

One thing falls out of reusing it. The template's host and path prefix *are* the
namespace, so a single config line gives both the link and the scope — and two
teams on different Jira instances cannot collide on PROJ-123. A second setting
for "which tracker" would have been a second way to say the same thing, and the
one that drifts.

There is no `PreToolUse` equivalent and none is needed: nothing fires when an
agent starts *thinking* about PROJ-123. The check happens when the claim is
taken, alongside the session-start block that already lists what peers hold.

**This is most of the board.** Work items need no storage here, and no
committed-versus-relay decision, because they already live in a tracker.
robofinger answers the one question a tracker answers worst — who is on this
right now — and leaves the content where it is.

### A channel is a tag people agree to read

Technically identical, socially different, and neither needs a crypto or relay
change. The fork that matters is whether a channel gates visibility.

It must not. Claims ignore groups deliberately, because a conflict warning some
peers cannot see is a warning that silently does not fire — and a channel that
hides entries reintroduces that everywhere. So **rooms carry who, channels carry
what**: one access mechanism, not two overlapping ones. That is the rule from
**Three layers of scope** applied to tags — noise is a filter, secrecy is a
group.

    relay.example.com/plan/acme     namespace = storage, room, directory
           #auth  #billing          channels = topic, audience-neutral

Channels never appear in a URL, because `#` is the age key fragment. `acme#auth`
is a display and CLI form; an address stays an address.

Tags are nearly free if they land with the filtered reads in 2 — a fourth axis
alongside path, kind and text, on the same code. The cheapest start is a hashtag
in the body, promoted to a field once filtering is actually used.

**The risk is proliferation, not access.** Five agents will invent `#auth`,
`#authentication` and `#auth-migration` inside a day. The fix is discovery rather
than governance: a `channels` listing of tags seen with counts, and a line in
`ROBOFINGER.md` telling an agent to look before it invents. Read before write,
the same shape as `since`.

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
  that cannot decrypt is silent — the worst property in the design. Rooms (6)
  answer it for members of a shared room; addressing anyone else still is not
  answered.
- **Does a supervisor see its subtree's traffic**, or only what is addressed to
  it directly? Unclear whether that is useful or noise. Blocks any move to make
  nested paths behave hierarchically rather than just read that way — see
  "Three layers of scope".
- **Loop prevention** if two nodes list each other as supervisor. Probably a hop
  count.
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
   ambient half worth reading. The provenance byline rides along with it: same
   file, same render path, and every entry written before it lands is an
   entry whose producer is unrecoverable.

8. **Non-consuming filtered reads** — by path, kind, text, tag. The retrieval
   mode, and what makes tags nearly free.

9. **Tasks** — last, because the tag version may fall out of 7–8 for free.

10. **Rooms** — gated by team size rather than by cost. Nothing above needs
    them and a team of five never will; they become urgent the first time
    onboarding one person means asking everyone else to run a command. Rooms
    also settle half of item 2: a shared roster lets a client check that an
    addressee can decrypt *before* publishing.

11. **Ticket claims** — barely an item: a tracker host in `project` and a
    positional that is not a glob. Independent of everything above, so they
    can land whenever someone wants them.

**1–2 are prerequisites for the claim being true; 3–6 are the claim.** 10–11 are
demanded by team size and by whatever tracker a team already runs, and can be
pulled in whenever either applies. 7–9 are the ambient half: real value, and the volume that makes a record worth reading,
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
