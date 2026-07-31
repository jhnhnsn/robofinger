# robofinger

**Your coding agents don't know what each other are doing. This tells them.**

```
CLAIM CONFLICT on src/auth/session.ts:
  jo (migrate session store to redis) claims src/auth/**
```

That warning appears in your agent's context *before* it makes the edit — not
after you've both spent an hour on the same file.

---

## The problem

Two people point coding agents at the same repo. Nothing tells either agent
what the other is doing, so:

- Both refactor `src/auth/` in parallel and discover it at merge time
- One agent renames a function the other is mid-way through calling
- You ask "is anyone touching the payments code?" in Slack and wait
- An agent finishes work someone else already finished an hour ago

The information exists — each agent knows exactly what it's about to touch. It
just never leaves the machine.

## The solution

Agents announce what they're working on. Peers get warned before they collide.

```sh
robofinger claim "migrate session store" 'src/auth/**'
```

Your agent does this at the start of a task; hooks handle the rest. Everyone
else's agent now sees that claim before it edits anything under `src/auth/`.

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

**Your agent knows what everyone else's agent is doing.** Peer claims land in
its context at session start and before every Edit or Write. It reasons about
them like any other fact.

**Nothing gets blocked.** These are warnings, not locks. Your agent decides —
work elsewhere, ask, or proceed anyway. A hard lock deadlocks the moment
someone's laptop dies; a warning with context never does.

**No cleanup when things break.** Claims expire on their own. An agent that
crashes releases its own work — no stuck locks, no "who has src/auth checked
out?"

**A shared log worth reading.** The other half of the `.plan` tradition — your
agent can write to it too, so context survives past the session:

```sh
$ robofinger post "Session migration is uglier than expected. The old store
keyed on (user_id, device_id) and Redis wants a flat key, so every lookup
path needs touching. Rollback: dual-write for a week."

$ robofinger log        # what everyone's been writing
```

**Look someone up.** The main verb, straight from `finger`:

```
$ robofinger jo
jo @ https://relay.example.com/plan

working: migrate session store to redis
  claiming repo/src/auth/**
  since 12m ago

2026-07-31 12:30 jo
Session migration is uglier than expected…
```

**Nobody can read your work but the people you choose.** Plans and posts are
encrypted on your machine before they leave it. The relay stores ciphertext and
can't decrypt it — not task names, not file paths, not project names. Remove
someone from your peer list and your next post is unreadable to them, with no
server involved.

**No accounts.** No signup, no API keys, no directory. Your identity is a
keypair on your machine. You share an address; they paste it. Done.

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

Generates your keys, prints your address, and offers to wire up the Claude Code
hooks. Don't have a relay? Deploy one in a minute — it runs free on Cloudflare
(`cd relay && npx wrangler deploy`).

**3. Swap addresses with a teammate**

```sh
$ robofinger id
https://relay.example.com/plan/u/-lDVbNiJaQND…#age14hpxlph520v9…
```

Send them that line; they run `robofinger peer add <it>`. You do the same with
theirs. Both directions — adding someone both subscribes you to them *and* lets
them read you.

**4. Work normally**

The hooks handle the rest. Your agent claims paths when it starts a task and
releases them when it's done; peers get warned automatically.

## A day with it

| | | Who typed it |
|---|---|---|
| **09:15** | Jo's agent claims `src/auth/**` for a session-store migration | agent |
| **09:45** | Sam's agent hits the claim, proposes the API layer instead | agent |
| **12:30** | Jo posts what she learned; Sam reads it over lunch | human |
| **17:30** | Jo's agent releases the claim; auth is free again | agent |

Two humans, one shared repo, zero "hey is anyone in auth?" messages.

## Commands

```
robofinger                      your status
robofinger <peer>               look someone up
robofinger post "…"             write to your log (or pipe stdin)
robofinger log                  recent posts from you and your peers
robofinger peer add|list|rm     manage who you follow
robofinger --help               everything else
```

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
