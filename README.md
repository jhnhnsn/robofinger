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

By default this publishes to `https://<worker>.<subdomain>.workers.dev`.

To serve it from your own domain instead, add a custom domain route to
`wrangler.jsonc` — wrangler creates the DNS record and TLS cert for you:

```jsonc
"workers_dev": false,
"routes": [
  { "pattern": "relay.example.com", "custom_domain": true }
]
```

The zone must already be on your Cloudflare account. Namespace data lives in a
Durable Object keyed by namespace name, so switching hostnames does not lose
plans.

### 2. Install the client

**While the repo is private** — needs the [GitHub CLI](https://cli.github.com),
authenticated once with `gh auth login`:

```sh
gh api repos/jhnhnsn/robofinger/contents/install.sh -H "Accept: application/vnd.github.raw" | sh
```

(`raw.githubusercontent.com` is also private, so the script has to come through
`gh` rather than plain `curl`.)

Detects your platform, downloads the matching release artifact, verifies its
sha256, and installs to `~/.local/bin` (override with `ROBOFINGER_BIN_DIR`).

**Once the repo is public**, the standard cargo-dist installer works with no
GitHub account:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/jhnhnsn/robofinger/releases/latest/download/robofinger-installer.sh | sh
```

Or build from source:

```sh
cd client && cargo build --release && cp target/release/robofinger ~/.local/bin/
```

Keep it current with `robofinger upgrade` (`--check` to look without installing).
It re-runs the published installer, and refuses to touch a copy installed by a
package manager.

### 3. Configure

```sh
robofinger init --url https://relay.example.com/plan
```

Writes `~/.config/robofinger/config`, generates keys on first use, and prints
your identity blob to share. Re-running it only changes the flags you pass.

Environment variables (`ROBOFINGER_URL`, `ROBOFINGER_NS`, `ROBOFINGER_AGENT`)
override the file when set — useful for CI or a one-off namespace.

Exchange identities with collaborators before publishing:

```sh
robofinger id                    # send this line to them
robofinger peer add rf1....      # add theirs
```

Both directions are required. Adding a peer subscribes you to their plans *and*
lets them decrypt yours; if only one side adds the other, the exchange is
half-open and claims silently fail to appear.

### 4. Wire into Claude Code

`robofinger init` offers to do this for you, asking whether to install at
**account** scope (`~/.claude/settings.json`, every project on this machine) or
**project** scope (`<repo>/.claude/settings.json`, this repo only — commit it so
teammates get the hooks on clone).

Or do it explicitly at any time:

```sh
robofinger hooks install              # account scope
robofinger hooks install --project    # this repo only
robofinger hooks uninstall            # remove, leaving other tools' hooks alone
```

The installer backs up `settings.json` first, preserves every other key, and is
idempotent. It writes the binary's absolute path, so hooks work even when
`~/.local/bin` is missing from the hook process's PATH.

To wire it by hand instead, merge into `~/.claude/settings.json`:

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
| `robofinger init --url <u> --ns <n>` | Write config, generate keys, offer hook install |
| `robofinger hooks install [--project]` | Wire into Claude Code (account or repo scope) |
| `robofinger id [label]` | Print your shareable identity blob |
| `robofinger peer add\|rm\|list\|update [-v]` | Manage trusted peers; `update` accepts a move |
| `robofinger moved <new address>` | Publish a signed forwarding pointer |
| `robofinger claim "<task>" <glob>...` | Publish a claim |
| `robofinger release` | Drop claims, stay working |
| `robofinger done` | Mark finished |
| `robofinger peers` | List live peer claims |
| `robofinger check <path>` | Conflict check (also reads hook JSON on stdin) |
| `robofinger post "<text>"` | Append to your log (also reads stdin) |
| `robofinger log [-n N] [--peer <label>]` | Recent posts from you and your peers |
| `robofinger read <label>` | One peer's posts, like fingering them |
| `robofinger watch` | Stream updates over WebSocket |
| `robofinger upgrade [--check]` | Update to the latest release |
| `robofinger --version` | Print version |

## Posts

`claim` is ephemeral state for machines; `post` is a durable log for humans —
the other half of the `.plan` legacy.

```sh
robofinger post "Spent the morning on the recipient list..."
git log --oneline -5 | robofinger post      # stdin also works
robofinger log                              # you and your peers, newest first
robofinger read alice                       # one peer, like fingering them
```

Posts are append-only, in their own table with their own seq space, so posting
never disturbs claim ordering. They are encrypted to exactly the same recipient
list as claims — your peers and nobody else. The relay stores ciphertext and
retains the most recent 500 per key.

## Addresses

An address is a URL:

```
https://relay.example.com/plan/u/<pubkey>?label=laptop#<agekey>
└──────── base = namespace ───┘    │          │          │
                                identity    label   encryption key
```

**The base path is the namespace.** A relay can live at `example.com/plan`
without colliding with the rest of the site, and `example.com/plan/team-a` is a
separate room with separate storage. There is no separate namespace field —
one URL says where and who.

**The age key sits in the fragment**, which browsers never send to servers. Paste
an address into a browser and the relay still cannot learn your encryption key.
That is a convention rather than a guarantee — only well-behaved relays are
bound by it — so confidentiality still rests on encryption, not on the fragment.

`robofinger id` prints yours; `peer add` takes anyone's. Peers on different
relays work transparently: the client groups them by base URL and queries each
relay it needs.

## Moving relay

Publish a signed pointer at your old address before you leave:

```sh
robofinger moved https://newhost.example.com/plan/u/<pubkey>#<agekey>
```

Peers see it in `peer list` but **are never redirected automatically** — a
stolen key would otherwise silently repoint them at an attacker's relay:

```
alice   39M2aVuUpbT6   old.example.com/plan        2m ago
        ↳ moved to https://newhost.example.com/plan/u/39M2…
          accept with: robofinger peer update alice
```

`peer update` verifies the pointer was signed by the same key that owns the old
address, and refuses a forward that names a *different* key — that would be an
identity swap, not a move. Pointers expire after a year.

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

### The address book

`~/.config/robofinger/peers` is one identity blob per line — plain text, hand
editable, `#` comments ignored. It does double duty: it is both your
**subscription list** (whose plans you fetch) and your **recipient list** (who
can decrypt yours), which is why revoking someone is a single `peer rm`.

```
$ robofinger peer list
alice          SL-fhgZqOqhq   relay.example.com/team-shared    36s ago
dave           jxfEhMAnrT-a   other-relay.example.com/their-ns    —
```

Last-seen comes from the plans and posts you can already decrypt, so it only
appears for peers who have added *you* as a recipient. A `—` means either they
have not published or you cannot read what they published.

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
- **Key rotation.** Not implemented. Changing identity means re-exchanging with
  every peer.

### Backing up keys

Losing `~/.config/robofinger/` means a new identity and re-exchanging with every
peer, so back the two key files up somewhere encrypted. Any password manager
works; below is [envstow](https://github.com/jhnhnsn/envstow), using a central
store so the keys never enter a git repo:

```sh
envstow init --store robofinger
export ENVSTOW_STORE=robofinger
envstow set ROBOFINGER_SIGNING_KEY < ~/.config/robofinger/signing.key
envstow set ROBOFINGER_AGE_KEY     < ~/.config/robofinger/age.key
```

Restore on a new machine:

```sh
export ENVSTOW_STORE=robofinger
mkdir -p ~/.config/robofinger
envstow run --only ROBOFINGER_SIGNING_KEY,ROBOFINGER_AGE_KEY -- sh -c '
  umask 077
  printf "%s" "$ROBOFINGER_SIGNING_KEY" > ~/.config/robofinger/signing.key
  printf "%s" "$ROBOFINGER_AGE_KEY"     > ~/.config/robofinger/age.key
'
robofinger id      # must print the same rf1... blob as before
```

Piping through stdin keeps the key material off the command line and out of
shell history. Verify a backup by restoring to a throwaway `ROBOFINGER_HOME`
and checking `robofinger id` matches — an untested backup is not a backup.

## Tests

```sh
cd client && cargo test        # matching, scoping, staleness, path handling
```

The path test is a regression guard: target files usually do not exist yet
(Write creates them), and on macOS git reports `/private/tmp` while hooks pass
`/tmp`. Both broke conflict detection during development.

## Design notes

- [docs/DEVICE-REGISTRATION.md](./docs/DEVICE-REGISTRATION.md) — how a hosted
  relay could gate access with Tailscale-style device grants, without the portal
  ever being able to read plans. Design only, not implemented.

## Limits (Cloudflare free tier)

100k DO requests/day, 5M rows read/day, 5GB storage.

| Operation | Requests | Rows read | Rows written |
|---|---|---|---|
| `check` | 1 | peers | 0 |
| `claim` / `release` / `done` | 2 | peers + 1 | 1 |
| `watch` connect | 1 | peers | 0 |

`check` fires on every Edit/Write and dominates everything else. At ~500 edits
per agent per day that is ~250 agents before the request limit binds.

Rows read scales with **peers you trust**, not with agents in the namespace:
`?from=` is applied in SQL against the `pubkey` primary key, so a client never
pays for plans it would discard. Clients with more than ~100 peers fall back to
an unfiltered fetch, because the URL would otherwise exceed what the edge
accepts.

Storage is one row per agent, overwritten in place — roughly 1KB each, so the
5GB limit is unreachable in practice.
