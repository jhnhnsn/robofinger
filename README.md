# robofinger

**`.plan` files for a world with agents.**

```
$ robofinger jo
jo @ https://relay.example.com/plan

working: migrate session store to redis
  claiming repo/src/auth/**
  since 12m ago

2026-07-31 12:30 jo
Session migration is uglier than expected. The old store keyed on
(user_id, device_id) and Redis wants a flat key, so every lookup path
needs touching. Rollback: dual-write for a week.

2026-07-30 18:04 jo
Finally got the dog to stop barking at the mail carrier. Six weeks of
treats. Unclear who trained whom.
```

---

## What a .plan was

Before status pages and standups, Unix had `finger`. Everyone kept a `.plan`
file in their home directory, and anyone could read it:

```
$ finger carmack@idsoftware.com
```

People wrote whatever they wanted in there. Carmack wrote engineering
journals that a generation of programmers read religiously. Others wrote
what they were stuck on, what they were reading, where they'd be Thursday.
It was a low-cost way to ping someone and see what they were up to —
**no notification, no reply expected, no performance.**

Then it died, and we replaced it with Slack statuses nobody reads and
standups everybody dreads.

## What robofinger is

The same idea, with two things added: **your agents can read it too**, and
**it's encrypted so only people you choose can.**

You write what you're working on. Your agent writes what it's touching.
Anyone you've shared keys with can look you up — and so can their agent,
which turns out to matter a lot when you both point coding agents at the
same repo.

```sh
robofinger post "Spent the morning fighting the recipient list. Also my
kid's science fair is Thursday so I'm out in the afternoon."
```

That's a blog post, a status update, and a heads-up — in one place, to
exactly the people you chose, with no platform in between.

## Why it matters when agents are involved

Two people point coding agents at the same repo. Nothing tells either agent
what the other is doing, so both refactor `src/auth/` in parallel and find
out at merge time.

The information exists — each agent knows exactly what it's about to touch.
It just never leaves the machine.

```sh
robofinger claim "migrate session store" 'src/auth/**'
```

Your agent does this when it starts a task. Everyone else's agent now sees
that claim before it edits anything under `src/auth/`.

## What it looks like in a session

Sam opens Claude Code. Before he types anything, his agent already knows what
Jo is doing:

> **SessionStart hook**
> ```
> Peer agent claims currently active:
>   jo claims repo/src/auth/** (migrate session store to redis)
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
> jo (migrate session store to redis) claims src/auth/**
> This is advisory. Consider working elsewhere, or coordinate first.
> ```
>
> **Claude:** Jo's mid-way through migrating the session store to Redis and has
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

**A place to think out loud.** Long-form, short-form, work, not-work. No
character limit, no algorithm, no audience anxiety. `robofinger post` takes
a sentence or an essay, and reads stdin so your tools can write to it too:

```sh
$ robofinger post "Rewrote the parser. Third time. This one's right."
$ git log --oneline -5 | robofinger post
```

**Reading someone is a poke, not a ping.** `robofinger jo` shows what she's
working on and what she's written lately. No notification fires on her end.
Nobody has to perform being busy. This is the part Slack got wrong.

**Your agent reads it too.** Peer claims land in its context at session start
and before every Edit or Write. It reasons about them like any other fact —
which is what stops two agents refactoring the same file.

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

**1. Install**

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/jhnhnsn/robofinger/releases/latest/download/robofinger-installer.sh | sh
```

<sub>While this repo is private, that URL 404s — use
`gh api repos/jhnhnsn/robofinger/contents/install.sh -H "Accept: application/vnd.github.raw" | sh`
instead, or build from source with `cd client && cargo build --release`.</sub>

**2. Point it at a relay**

```sh
robofinger init --url https://relay.example.com/plan
```

Generates your keys and prints your address. Don't have a relay? Deploy one in
a minute — it runs free on Cloudflare (`cd relay && npx wrangler deploy`).

To let your coding agent read and write your plan, install the hooks — either
during `init --hooks`, or any time after:

```sh
robofinger hooks install              # this machine
robofinger hooks install --project    # just this repo, commit to share
```

**3. Swap addresses with a teammate**

```sh
$ robofinger id
https://relay.example.com/plan/u/-lDVbNiJaQND…#age14hpxlph520v9…
```

Send them that line; they run `robofinger add <it>`. You do the same with
theirs. Both directions — adding someone both subscribes you to them *and* lets
them read you.

**4. Work normally**

The hooks handle the rest. Your agent claims paths when it starts a task and
releases them when it's done; peers get warned automatically.

## A day with it

| | | |
|---|---|---|
| **09:15** | Jo's agent claims `src/auth/**` for a session-store migration | agent |
| **09:45** | Sam's agent hits the claim, proposes the API layer instead | agent |
| **12:30** | Jo posts what she learned, and that she's out Thursday | Jo |
| **14:00** | Sam reads it over lunch. Doesn't reply. Doesn't need to. | Sam |
| **17:30** | Jo's agent releases the claim; auth is free again | agent |

Two humans, one shared repo, zero "hey is anyone in auth?" messages and zero
notifications.

## Commands

```
robofinger <peer>               read someone's .plan
robofinger                      read your own
robofinger post "…"             write to it (or pipe stdin)
robofinger log                  everyone you follow, newest first
robofinger add <address>        follow someone
robofinger list                 who you follow, and what they hold
robofinger --help               everything else
```

Your agent uses `claim`, `release` and `check` through the hooks. You mostly
won't type those.

## Good to know

**Claiming is best-effort.** The hooks always run, but *publishing* a claim is
an instruction your agent follows. If it skips one, peers see nothing — you
lose a warning, you don't break anything.

**~95ms per edit.** That's the relay round trip on each Edit/Write.

**Self-host anything you like.** The relay is one small Worker on Cloudflare's free
tier, and the client talks to any relay. Your keys never leave your machine.

## More

- [Reference](docs/REFERENCE.md) — protocol, crypto, limits, operations
- [Device registration](docs/DEVICE-REGISTRATION.md) — design notes for hosted access

[MIT](LICENSE)
