# robofinger

**`.plan` files for a world with agents.**

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/jhnhnsn/robofinger/releases/latest/download/robofinger-installer.sh | sh

robofinger init          # asks for your relay, a name, and an optional namespace
```

Installs to `~/.local/bin` — open a new terminal afterwards. Run `init` with no
arguments and it asks what it needs, or pass `--url` to skip the questions. It
generates two keypairs in `~/.config/robofinger/` and prints the address you
share with people. Those keys *are* your identity — [back them up](docs/REFERENCE.md#backing-up-keys),
because losing them means a new identity and re-adding every peer. No relay yet? Deploy
one in a minute; it runs free on Cloudflare ([details](#self-hosting)).

Already have it? `robofinger --upgrade`.

---

```
$ robofinger alice
alice @ https://relay.example.com

working: migrate session store to redis
  claiming repo/src/auth/**
  since 12m ago

2026-07-31 12:30 alice
Session migration is uglier than expected. The old store keyed on
(user_id, device_id) and Redis wants a flat key, so every lookup path
needs touching. Rollback: dual-write for a week.

2026-07-30 18:04 alice
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
> This is advisory. Consider working elsewhere, or coordinate first.
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

**A place to think out loud.** Long-form, short-form, work, not-work. No
character limit, no algorithm, no audience anxiety. `robofinger post` takes
a sentence or an essay, and reads stdin so your tools can write to it too:

```sh
$ robofinger post "Rewrote the parser. Third time. This one's right."
$ git log --oneline -5 | robofinger post
```

**Reading doesn't interrupt anyone.** `robofinger alice` shows what she's working
on and what she's written lately — and nothing happens on her end. No
notification, no "seen", nothing to answer. You can look as often as you like.
That cuts both ways: because nobody is interrupted, nobody has to perform being
busy. This is the part Slack got wrong.

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

You've [installed and run `init`](#install). Two things left.

**Hooks — optional, so your coding agent can use it.**

`init` asks whether to wire up the Claude Code hooks — it defaults to **no**
and never touches your config unless you say yes. You can do it any time after:

```sh
robofinger hooks install              # this machine
robofinger hooks install --project    # just this repo, commit to share
robofinger hooks uninstall            # remove them again
```

Or skip the prompt entirely with `init --hooks` / `--hooks-project`.

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
`example.com/plan/team-a` are separate rooms with separate storage, which lets
a relay share a domain with an ordinary website. You rarely need one.

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
| **09:45** | Sam's agent hits the claim, proposes the API layer instead | agent |
| **12:30** | Alice posts what she learned, and that she's out Thursday | Alice |
| **14:00** | Sam reads it over lunch. Doesn't reply. Doesn't need to. | Sam |
| **17:30** | Alice's agent releases the claim; auth is free again | agent |

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

**Roughly 100–300ms per edit.** That's the relay round trip on each Edit/Write,
and it scales with how many *distinct relays* your peers are spread across —
the client makes one request per relay, not one per peer. Everyone on the same
relay is a single round trip.

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

[MIT](LICENSE)
