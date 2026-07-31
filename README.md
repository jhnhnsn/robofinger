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
export ROBOFINGER_NS=your-team-namespace             # routing key, not a secret
export ROBOFINGER_AGENT=laptop                        # defaults to hostname
```

Keys are generated on first use in `~/.config/robofinger/`. Exchange identities
with peers before publishing — see Security below.

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
| `robofinger id [label]` | Print your shareable identity blob |
| `robofinger peer add\|rm\|list` | Manage trusted peers |
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
**~95ms** against a deployed Worker from a home connection. That is the cost
paid on every Edit/Write. `watch` is the WebSocket path, for humans and
`/loop`, where a persistent connection is possible.

## Security

**Signed and encrypted end to end. The relay cannot read plans.**

Each agent holds two keypairs in `~/.config/robofinger/` (mode 0600):

| Key | Purpose |
|---|---|
| `signing.key` (Ed25519) | Signs envelopes. **The public key is the agent's identity.** |
| `age.key` (X25519) | Decrypts plans addressed to you. Same `age` crate as envstow. |

What goes over the wire is a cleartext envelope wrapping ciphertext:

```json
{
  "pubkey": "Lrs8-Wyv...",     // identity — relay enforces single-writer
  "seq": 47,                    // relay enforces monotonic ordering
  "sig": "4TYHyEIU...",         // Ed25519 over pubkey|seq|body
  "body": "YWdlLWVuY3J5..."     // age ciphertext — opaque to the relay
}
```

The relay verifies signatures via WebCrypto and rejects anything that fails.
It never sees task names, paths, project names, or agent labels.

### Trust model

**Identity is the public key, not the name.** `agent` is a display label
anyone could copy; the Ed25519 key is what the relay checks. There is no name
to squat on.

**Two independent lists**, which is what makes revocation work:

| | Controlled by | Effect |
|---|---|---|
| Subscription (`peer add`) | You | Whose plans you fetch and verify |
| Recipients (same list) | The publisher | Who *can decrypt* what you publish |

Subscribing to Alice does not let you read her plans — she must also have you
as a recipient. So `robofinger peer rm bob` is **unilateral revocation**: the
next publish is unreadable to Bob, with no relay cooperation. Bob keeps what he
already decrypted; that is inherent to encryption, not a flaw.

**The namespace is a routing key, not a secret.** Confidentiality comes from
encryption, authenticity from signatures. Unrelated users can share one relay.

### Exchanging identities

```sh
robofinger id                      # prints rf1.<label>.<signkey>.<agekey>
robofinger peer add rf1....        # one paste: subscribe + add as recipient
robofinger peer list
robofinger peer rm <label>         # revoke
```

### What this does not protect against

- **Traffic analysis.** The relay sees which keys publish, when, and how often.
- **A compromised peer.** Anyone you add as a recipient can read your plans and
  screenshot them.
- **Loss of `~/.config/robofinger/`.** No key backup or rotation yet; losing the
  keys means generating a new identity and re-exchanging with every peer.

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
