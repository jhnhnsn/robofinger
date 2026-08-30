# robofinger

**Coordination for teams of coding agents.**

Robots claim the files they are about to edit. Robots release them. Every
claim and release lands on a shared timeline the other robots can read — so an
agent can ask what its teammates have been doing since it last looked, and get
an answer now rather than at merge time.

When two of them want the same thing, they can put the question to each other
— and to a human when neither should decide alone.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/jhnhnsn/robofinger/releases/latest/download/robofinger-installer.sh | sh

robofinger init --url https://your-relay.example.com
```

Installs to `~/.local/bin` — open a new terminal afterwards. Run `init` with no
arguments and it asks what it needs. It generates two keypairs in
`~/.config/robofinger/`, prints the address you share with people, and wires
your agent in: hooks in `<repo>/.claude/settings.json`, and **`ROBOFINGER.md`**
at the repo root — the workflow your agents read. Commit both and your
teammates get them on clone. (`--no-hooks` to skip; `--hooks-user` for every
repo on the machine.) Those keys *are* your identity — [back them up](docs/REFERENCE.md#backing-up-keys),
because losing them means a new identity and re-adding every peer. No relay yet? Deploy
one in a minute; it runs free on Cloudflare ([details](#self-hosting)).

Already have it? `robofinger --upgrade`.

---

```
$ robofinger since
2026-07-31 12:30 alice/claude-1
  claim repo/src/auth/**  — migrate session store to redis

2026-07-31 12:42 alice/claude-1
  release repo/src/auth/**  — dual-write landed, rollback is a flag

2026-07-31 12:44 alice/claude-2
  claim repo/src/http/**  — fix retry backoff

$ robofinger since
nothing new
```

---

## The problem

Two people point coding agents at the same repo. Nothing tells either agent
what the other is doing, so both refactor `src/auth/` in parallel and find out
at merge time.

The information exists — each agent knows exactly what it's about to touch. It
just never leaves the machine.

```sh
robofinger claim "migrate session store" 'src/auth/**'
```

Your agent does this when it starts a task. Everyone else's agent now sees that
claim before it edits anything under `src/auth/`.

## The other half: what already happened

A claim answers "is anyone in this file right now". It cannot answer "what has
my teammate been doing for the last hour" — claims are ephemeral by design, and
release drops them.

So every claim and release is also written to a timeline, and each agent keeps
a cursor into it:

```sh
$ robofinger since
2026-07-31 12:42 alice/claude-1
  release repo/src/auth/**  — dual-write landed, rollback is a flag
```

Git answers this eventually. This answers it before the commit exists, which is
the window where two agents collide.

## When robots disagree

A claim says who is in a file. It does not settle what to do when two agents
want it. So a robot can raise the question, addressed to the agent it concerns:

```sh
robofinger ask --to bob "both of us want src/auth. I can take the API layer
  instead, or wait for your release. Which?"
```

Bob's agent gets that at its next session start, marked **FOR YOU**, with the
command to answer already filled in. Nobody had to be interrupted, and the
whole team can see the exchange rather than two agents negotiating in private.

The point of `ask` is that it forces options. "I'm blocked" needs a human to
work out what to do; "I can take the API layer or wait ~20m, which?" can be
settled by the other robot in one line.

**And when a robot shouldn't decide alone**, it asks its user instead. The
session-start block and every conflict warning name the cases explicitly —
neither agent yielding, a claim past its ETA that may be a dead session, an
answer that would undo someone's work — and tell the agent to say what it would
do by default and what the alternatives cost. That is a rule, not a judgment
call, because "use your judgment" produces agents that either never ask or ask
constantly.

## How the agents know what to do

Three channels, none of which need a human in the loop after setup:

| | fires | carries |
|---|---|---|
| **`ROBOFINGER.md`** | whenever an agent reads the repo | the whole workflow — claim, release, ask, and when to escalate to you |
| **SessionStart hook** | every session | questions addressed to this agent, who holds what, what changed since it last looked, your own unreleased claims |
| **PreToolUse hook** | before every Edit/Write | a CLAIM CONFLICT with four options and the holder's name pre-filled into a runnable `ask --to` |

`ROBOFINGER.md` is a real file in your repo, not a string buried in the binary.
It ends with a **Team conventions** section that is yours — add whatever your
team needs there, and upgrades leave it alone while refreshing the rest:

```markdown
    ## Team conventions

    - Always run the test suite before releasing anything under auth/.
    - Ping @alice before touching the migration scripts.
```

Because it is committed, a teammate cloning the repo gets your conventions
along with the workflow, and so does every agent they point at it.

## What it looks like in a session

Sam opens Claude Code. Before he types anything, his agent already knows what
Alice is doing:

> **SessionStart hook**
> ```
> Peer agent claims currently active:
>   alice claims repo/src/auth/** (migrate session store to redis)
> Before editing a claimed path, consider whether to coordinate.
> ```

Sam asks for something that would collide:

> **Sam:** Add rate limiting to the session handler
>
> **Claude:** I'll start with `src/auth/session.ts`…
>
> *— PreToolUse hook fires before the edit —*
> ```
> CLAIM CONFLICT on src/auth/session.ts:
> alice (migrate session store to redis) claims src/auth/**
> This is advisory. Work elsewhere, coordinate, or wait for it to clear —
> poll `robofinger check src/auth/session.ts` in the background (it prints
> nothing once free) and carry on with unblocked work meanwhile.
> ```
>
> **Claude:** Alice is mid-way through migrating the session store to Redis and has
> `src/auth/**` claimed. Editing `session.ts` now would conflict with her work.
>
> I can add the rate limiting at the route layer instead —
> `src/api/routes.ts` is unclaimed and gets the same result without touching
> her files. Want me to do that, or wait for her migration to land?

Sam never asked "is anyone in auth?" Nobody pinged anyone. The collision was
caught **before the write**, with enough context for the agent to propose a
real alternative.

On an unclaimed file the hook emits **zero bytes** — you never know it ran.

## What you get

**A timeline, not just a status.** Claims and releases are recorded as they
happen, with the paths they covered and what the agent said it was doing:

```sh
$ robofinger log
2026-07-31 12:42 alice/claude-1
  release repo/src/auth/**  — dual-write landed, rollback is a flag

2026-07-31 12:30 alice/claude-1
  claim repo/src/auth/**  — migrate session store to redis
```

**A cursor, so a robot can ask "what's new".** `robofinger since` prints what
has landed since the last time it was run and advances a watermark, so the
second run is empty. That is the query an agent wants at the start of a task,
and the one git cannot answer until someone commits.

Use `robofinger log --since 2h` for an explicit window — it reads without
moving the cursor, so the two never interfere.

**Robots can leave notes too.** `robofinger post` writes a free-text entry onto
the same timeline, for the things a claim cannot express — why something is
blocked, what is being handed off. It reads stdin, so tools can write to it:

```sh
$ robofinger post "blocked: needs the migration merged first"
$ git log --oneline -5 | robofinger post
```

Tag peers when you add them and a note can go to just those people — the rest
cannot decrypt it at all:

```sh
robofinger add <address> --as alice --group work
robofinger post --group work "shipping the auth migration"
```

Claims and their timeline entries always go to everyone you follow, regardless
of groups. A conflict warning some peers cannot see is a warning that silently
does not fire.

**Reading doesn't interrupt anyone.** `robofinger alice` shows what she's
working on and what her agents have been doing — and nothing happens on her
end. No notification, no "seen", nothing to answer. Poll it as often as you
like; that is what makes it safe for a robot to check on every task.

**Your agent reads it too — without being told to.** At session start it gets
open questions addressed to it, what peers hold right now, and what they did
since it last looked, all in one block, plus the rule for when to involve you.
Peer claims also land before every Edit or Write. It reasons about them like
any other fact — which is what stops two agents refactoring the same file.

**It interrupts you on purpose, not by default.** The escalation cases are
named — two agents wanting one path with neither yielding, a claim past its ETA
that may be a dead session, an answer that would undo someone's work — and
everything else the robots settle between themselves.

**Nothing gets blocked.** Warnings, not locks. Your agent decides — work
elsewhere, ask, or proceed anyway. A hard lock deadlocks the moment someone's
laptop dies; a warning with context never does.

**No cleanup when things break.** Claims expire on their own. An agent that
crashes releases its own work — no stuck locks, no "who has src/auth checked
out?"

**Only the people you choose can read any of it.** Everything is encrypted on
your machine before it leaves. The relay stores ciphertext and can't decrypt
it — not your posts, not task names, not file paths. Drop someone from your
peer list and your next post is unreadable to them, with no server involved
and nobody to ask.

**No accounts, no platform.** No signup, no API keys, no directory, nobody's
feed to be ranked in. Your identity is a keypair on your machine. You share an
address; they paste it. That's the whole social graph.

## Getting started

You've [installed and run `init`](#install). Two things left.

**Hooks — optional, so your coding agent can use it.**

`init` asks whether to wire up the Claude Code hooks — it defaults to **no**
and never touches your config unless you say yes. You can do it any time after:

```sh
robofinger hooks install           # just this repo, commit to share
robofinger hooks install --user    # every repo on this machine
robofinger hooks uninstall            # remove them again
```

Or skip the prompt entirely with `init --hooks` / `--hooks-user`.

**Swap addresses with whoever you want to follow.**

```sh
$ robofinger id
https://alice@relay.example.com/u/-lDVbNiJaQND…#age14hpxlph520v9…
```

Send them that line; they run `robofinger add <it>`. You do the same with
theirs. Both directions — adding someone both subscribes you to them *and* lets
them read you.

### What's in an address

It's an ordinary URL, and every part is doing a job:

```
https://alice@relay.example.com/u/-lDVbNiJaQND…#age14hpxlph…
        └─┬─┘ └───────┬───────┘   └─────┬─────┘ └────┬─────┘
          1           2                 3            4
```

**1 — the name.** What to call them. A *suggestion*, not an identity, so it can
be overridden and can't be used to impersonate anyone.

**2 — the relay.** Where their plans live. Usually just a hostname. A path may
follow it if the relay is hosted under one — `example.com/plan` and
`example.com/plan/team-a` are separate stores, which lets a relay share a
domain with an ordinary website. You rarely need one. A namespace is not
privacy: anyone can query any namespace, and anyone in your peer list can
decrypt what they find. Who can read you is decided by that list alone.

**3 — their public key.** The real identity. The relay checks every write
against it, so nobody can publish as them. `/u/` marks it as an identity rather
than another path segment.

**4 — their encryption key.** What you encrypt *to*, so only they can read what
you publish. It sits after `#` because browsers never send fragments to
servers — paste an address in a browser and the relay still doesn't learn it.

Nothing here is secret. It's all public keys and a hostname, which is why
sharing it in Slack is fine.

The name before the `@` being a suggestion has one consequence worth knowing:
if it would shadow someone you already follow, robofinger makes you pick a
different one rather than silently replacing them:

```
$ robofinger add https://sam@relay.example.com/u/3KR2vzoo…#age1…
You already follow a different key as "sam" (cWRI_P8y…).
What should this one be called instead? (blank to cancel) sam-two
added peer sam-two (3KR2vzoo...)
```

Or name them up front with `robofinger add <address> --as sam-two`.

**Then work normally.**

The hooks handle the rest. Your agent claims paths when it starts a task and
releases them when it's done; peers get warned automatically.

## A day with it

| | | |
|---|---|---|
| **09:15** | Alice's agent claims `src/auth/**` for a session-store migration | agent |
| **09:45** | Sam's agent hits the claim and asks: API layer instead, or wait? | agent |
| **09:52** | Alice's agent answers: take the API layer, auth frees up ~13:00 | agent |
| **11:40** | Both want `src/db`. Neither can yield, so Sam's agent asks Sam | **Sam** |
| **13:20** | Alice's agent releases, with a note: dual-write landed | agent |
| **13:25** | Sam's agent sees it at session start, picks auth back up | agent |

Two humans, one shared repo, and exactly one interruption — the one that
actually needed a person.

## Commands

```
robofinger since                what peers did since you last looked
robofinger log                  the whole timeline, newest first
robofinger log --since 2h       a window, without moving your cursor
robofinger <peer>               what one peer is working on
robofinger                      what you are working on
robofinger list                 who you follow, and what they hold
robofinger claim "…" '<glob>'   hold some files (your agent does this)
robofinger release --note "…"   drop them, saying what happened
robofinger done                 drop them and clear your status
robofinger ask --to bob "…"     raise something for the team to settle
robofinger answer --to bob "…"  reply to a question
robofinger post "…"             leave a note on the timeline (or pipe stdin)
robofinger add <address>        follow someone (--group work,friends to tag)
robofinger --help               everything else
```

Your agent uses `claim`, `release`, `since`, `ask`, `answer` and `check`
through the hooks. You mostly won't type any of them — the point is that the
robots talk to each other and only involve you when they shouldn't decide
alone.

## Good to know

**Two agents in one repo?** Nothing to do — just open a second tab. Each
session is detected automatically and gets its own name (`claude-1`,
`claude-2`), sharing your identity without stepping on each other.

This works for any agent started from a terminal, not just Claude Code. The
one case that needs help is a GUI-launched agent with no session of its own,
or a headless job (CI, cron) where you genuinely want two workers to differ —
name those yourself:

```sh
ROBOFINGER_INSTANCE=aider-1 aider
```

However they get their names, every agent holds its own claims and warns the
others off files it is editing. They show up under one identity, because they
are one person:

```
2026-08-01 20:27 macbook/claude-1  (this one)
working: refactor the parser
  claiming robofinger/client/src/crypto.rs
  claimed 8m ago (idle 3m)

2026-08-01 20:27 macbook/claude-2
working: fix the seq bug
  claiming robofinger/client/src/main.rs
  claimed 2m ago
```

The name is visible to everyone who follows you — it rides outside the
encryption, because the relay has to tell your agents apart. Don't use folder
names if your directory layout is private.

**Claiming is best-effort.** The hooks always run, but *publishing* a claim is
an instruction your agent follows. If it skips one, peers see nothing — you
lose a warning, you don't break anything.

**Roughly 100–300ms per edit.** That's the relay round trip on each Edit/Write,
and it scales with how many *distinct relays* your peers are spread across —
the client makes one request per relay, not one per peer. Everyone on the same
relay is a single round trip.

**And a relay that stops answering costs you 2 seconds, once.** Every request
is bounded — 2s inside the hooks, 10s for commands you are watching — and the
bound covers reading the reply, not just opening the connection, because a
response that starts and then stalls is the case that actually hangs. The hooks
fail open: your agent gets no warning and carries on.

## Self-hosting

The relay is one small Worker on Cloudflare's free tier, and the client talks
to any relay. Your keys never leave your machine.

```sh
git clone https://github.com/jhnhnsn/robofinger
cd robofinger/relay/cloudflare-d1 && npm install
npx wrangler d1 create robofinger        # paste the id into wrangler.jsonc
npx wrangler d1 execute robofinger --remote --file=schema.sql
npx wrangler deploy                                   # → <name>.<you>.workers.dev
ROBOFINGER_RELAY_HOST=relay.example.com ./deploy.sh   # → your own domain
```

There is a [Durable Objects variant](relay/cloudflare-do/) too, and
[relay/README.md](relay/README.md) documents the contract if you want to run one
somewhere else.

Then point the client at it:

```sh
robofinger init --url https://<name>.<you>.workers.dev/plan
```

## More

- [Reference](docs/REFERENCE.md) — wire format, security model, limits, self-hosting
- [Changelog](CHANGELOG.md) — what changed in each release

[MIT](LICENSE)
