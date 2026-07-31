# robofinger

Real-time plan sync for coding agents. Each agent publishes what it is working
on and which paths it is touching; peers get pushed the update and are warned
before they edit a claimed file.

Two pieces:

- `relay/` — Cloudflare Worker + Durable Object. Stores the latest plan per
  agent, fans out over WebSocket. Runs on the **free tier** (SQLite-backed DO).
- `client/` — Rust CLI. Publishes claims, checks paths, streams updates.
  Wires into Claude Code as hooks.

## The idea

Agents clobber each other because nothing tells them what the others are doing.
Three mechanisms fix that, in increasing cost:

1. **Single-writer keys** — an agent may only write `plan/<its own id>`. The
   relay rejects anything else, so no plan can ever be overwritten by a peer.
2. **Monotonic seq** — a plan with `seq <= last_seen` is rejected. Kills
   replays and out-of-order delivery.
3. **Advisory claims** — `touching` lists globs. Before editing, an agent
   checks for overlap and is warned. Advisory on purpose: a hard lock deadlocks
   when an agent dies mid-task.

Claims expire at `ts + eta_s * 2`. A crashed agent releases its own claims —
no heartbeat protocol, no tombstones.

## Setup

### 1. Deploy the relay

```sh
cd relay
npx wrangler deploy
```

Note the URL, e.g. `https://robofinger.you.workers.dev`.

### 2. Build the client

```sh
cd client
cargo build --release
cp target/release/robofinger ~/.local/bin/
```

### 3. Configure

```sh
export ROBOFINGER_URL=https://robofinger.you.workers.dev
export ROBOFINGER_NS=your-team-shared-secret-string   # 8-128 chars
export ROBOFINGER_AGENT=laptop                        # defaults to hostname
```

The namespace **is** the access control right now — anyone who knows it can
read and write plans in it. Treat it like a password. See Security below.

### 4. Wire into Claude Code

Merge into `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "robofinger start" }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "robofinger end" }] }
    ],
    "PreToolUse": [
      {
        "matcher": "Edit|Write|NotebookEdit",
        "hooks": [{ "type": "command", "command": "robofinger check" }]
      }
    ]
  }
}
```

Add to `~/.claude/CLAUDE.md` so the agent actually publishes claims:

```markdown
## Agent Plan Sync
Before editing files, claim the paths:
`robofinger claim "<task>" '<glob>' '<glob>'`
Release when done: `robofinger release`
A CLAIM CONFLICT warning means another agent is working there — decide whether
to back off, don't silently ignore it.
```

## Commands

| Command | Does |
|---|---|
| `robofinger claim "<task>" <glob>...` | Publish a claim |
| `robofinger release` | Drop claims, stay working |
| `robofinger done` | Mark finished |
| `robofinger peers` | List live peer claims |
| `robofinger check <path>` | Conflict check (also reads hook JSON on stdin) |
| `robofinger watch` | Stream updates over WebSocket |

## Plan format

```json
{
  "agent": "laptop",
  "seq": 47,
  "epoch": 1785463606,
  "status": "working",
  "task": "refactor auth middleware",
  "touching": ["src/auth/**", "src/middleware.ts"],
  "project": "studybuddy",
  "eta_s": 1800
}
```

`project` is the git repo name, so `src/**` in one repo never conflicts with
`src/**` in another.

## Why the hooks poll instead of subscribe

`check` runs per tool call and exits. A process that lives for one command
cannot hold a WebSocket open, so `check` does a single HTTP GET — measured
**~260ms** against a deployed Worker from a home connection. That is the cost
paid on every Edit/Write. `watch` is the WebSocket path, for humans and
`/loop`, where a persistent connection is possible.

## Security

**Plaintext, namespace-as-password.** The relay operator can read every plan.
That is fine when you deploy it to your own Cloudflare account and the plans
say things like "refactoring auth middleware".

Not yet built: payload encryption (age/NaCl multi-recipient, so the relay never
sees plaintext), signatures, per-agent auth tokens. The design supports all
three — the relay treats the body as opaque and only reads `agent` and `seq`
from the envelope.

## Tests

```sh
cd client && cargo test        # matching, scoping, staleness, path handling
```

The path test is a regression guard: target files usually do not exist yet
(Write creates them), and on macOS git reports `/private/tmp` while hooks pass
`/tmp`. Both broke conflict detection during development.

## Limits (Cloudflare free tier)

100k DO requests/day, 5GB storage. A plan update is one request; a `check` is
one request. Well inside the free tier for normal use.
