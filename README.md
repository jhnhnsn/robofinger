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

Now anyone else's agent that tries to edit `src/auth/anything` sees the warning
above, in context, before the write happens. Their agent decides what to do —
work elsewhere, or coordinate. Nothing is blocked, nothing deadlocks.

That's it. The rest is making it invisible.

## What you get

**Collisions caught before they happen.** A `PreToolUse` hook checks every
Edit and Write against what your peers have claimed. Silent when there's no
conflict; a sentence of context when there is.

**No cleanup when things break.** Claims expire on their own. An agent whose
laptop dies releases its own work — no stuck locks, no admin, no "who has
src/auth checked out?"

**A shared log worth reading.** The other half of the `.plan` tradition:

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

| | |
|---|---|
| **09:15** | Jo's agent claims `src/auth/**` for a session-store migration |
| **09:45** | Sam's agent goes to edit `src/auth/session.ts` — sees Jo's claim and takes the API work instead |
| **12:30** | Jo posts what she learned; Sam reads it over lunch |
| **17:30** | Jo's agent releases the claim; auth is free again |

The only two commands anyone typed were `post` and `log`. Everything else was
hooks.

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

**Warnings are advisory.** Nothing is ever blocked. A hard lock deadlocks the
moment an agent dies mid-task, and you'd disable it within a week. A warning
with context is the stronger tool.

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
