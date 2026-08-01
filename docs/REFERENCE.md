# robofinger — reference

Wire format, security model and operational limits. For what robofinger is and
how to use it, see the [README](../README.md); for the command surface, run
`robofinger --help`.

## Addresses

```
https://sam@relay.example.com/plan/u/<pubkey>#<agekey>
       │    └────── base = namespace ──┘   │       │
  suggested label                      identity  encryption key
```

**The base path is the namespace.** A relay can live at `example.com/plan`
alongside the rest of the site, and `example.com/plan/team-a` is a separate
room with separate storage. There is no namespace field — one URL says where
and who.

**The age key sits in the fragment**, which browsers never send to servers.
That is a convention rather than a guarantee — only well-behaved relays are
bound by it — so confidentiality rests on encryption, not on the fragment.

**The label before `@` is a suggestion, not identity.** The public key is the
identity. A suggested label that would shadow an existing peer is refused
rather than silently replacing them. `?label=` is still parsed for older
addresses.

## Wire format

Two layers. The relay sees the envelope; only recipients see the plan.

### Envelope (cleartext)

```json
{
  "pubkey": "fHC-SO9SYAjaB29D8DIRTxTNF2LaicTKk046FtGSIkk",
  "seq": 47,
  "sig": "o4AcVux_bUyWwO1Mo1RwB1i0…",
  "body": "YWdlLWVuY3J5cHRpb24ub3JnL3Yx…"
}
```

| Field | Purpose |
|---|---|
| `pubkey` | Ed25519 identity; the relay accepts writes only to this key's own path |
| `seq` | Monotonic per key; a write with `seq <= stored` is rejected 409 |
| `sig` | Ed25519 over `pubkey\|seq\|body`, so neither ordering nor ciphertext can be altered in transit |
| `body` | base64url of an age ciphertext — opaque to the relay |

### Plan (encrypted, inside `body`)

Compact UTF-8 JSON. No schema language, no binary framing — the shape comes
from the `Plan` struct in `client/src/main.rs`, which is the only definition.

```json
{"agent":"laptop","pubkey":"fHC-SO9S…","seq":47,"epoch":1785549052,
 "status":"working","task":"refactor auth middleware",
 "touching":["src/auth/**"],"project":"myrepo","eta_s":1800}
```

| Field | Meaning |
|---|---|
| `agent` | display label only — not identity |
| `pubkey` | echoed inside; the client overwrites it from the signed envelope |
| `seq`, `epoch` | ordering and publish time (unix seconds) |
| `status` | `working` \| `done` \| `post` \| `moved` |
| `task` | free text — a claim description, or the body of a post |
| `touching` | globs, relative to the repo root; empty means no claim |
| `project` | git repo name, so `src/**` in one repo cannot conflict with another |
| `eta_s` | staleness budget; a claim is live while `now - epoch < eta_s * 2` |

Every field is `#[serde(default)]`, so an older client reading a newer plan
drops what it does not recognise rather than failing.

## Endpoints

All paths are relative to the relay base URL, whose path is the namespace.

| Method | Path | Does |
|---|---|---|
| GET | `/plans?from=<keys>` | current claims for the listed keys |
| GET | `/posts?from=<keys>&limit=N&before=<id>` | newest-first posts |
| GET | `/u/<pubkey>` | one identity: plans, posts and forward together |
| GET | `/forward/<pubkey>` | a signed "I moved" pointer, if any |
| PUT | `/plan/<pubkey>` | publish a claim |
| PUT | `/post/<pubkey>` | append a post |
| PUT | `/forward/<pubkey>` | publish a forwarding pointer |
| GET | `/subscribe?from=<keys>` | WebSocket: snapshot then live pushes |

`?from=` is applied in SQL against the `pubkey` primary key, so a client never
pays to read plans it would discard.

## Security

Each agent holds two keypairs in `~/.config/robofinger/`, mode 0600:

| Key | Job |
|---|---|
| `signing.key` (Ed25519) | signs envelopes; **the public key is the identity** |
| `age.key` (X25519) | decrypts plans addressed to you |

Two keys because signing and encryption are different operations — Ed25519
cannot encrypt and X25519 cannot sign.

### Trust model

**Identity is the public key, not the name.** The relay verifies every write
against it, so there is no name to squat on.

**Subscription and readability are separate**, which is what makes revocation
work:

| | Controlled by | Effect |
|---|---|---|
| `robofinger add` | you | whose plans you fetch and verify |
| recipient list (same file) | the publisher | who *can decrypt* what you publish |

Following someone does not let you read them — they must also have you as a
recipient. So `robofinger rm bob` is unilateral revocation: the next publish is
unreadable to Bob, with no relay cooperation. He keeps what he already
decrypted; that is inherent to encryption.

**The namespace is a routing key, not a secret.** Unrelated users can share one
relay.

### What this does not protect against

- **Traffic analysis.** The relay sees which keys publish, when, and how often.
- **A compromised peer.** Anyone you add can read your plans.
- **Key rotation.** Not implemented; changing identity means re-exchanging.
- **Losing `~/.config/robofinger/`.** Back the two key files up — see below.

### Backing up keys

Any encrypted store works. With [envstow](https://github.com/jhnhnsn/envstow),
using a central store so keys never enter a git repo:

```sh
envstow init --store robofinger
export ENVSTOW_STORE=robofinger
envstow set ROBOFINGER_SIGNING_KEY < ~/.config/robofinger/signing.key
envstow set ROBOFINGER_AGE_KEY     < ~/.config/robofinger/age.key
```

Restore:

```sh
envstow run --store robofinger --only ROBOFINGER_SIGNING_KEY,ROBOFINGER_AGE_KEY -- sh -c '
  umask 077
  printf "%s" "$ROBOFINGER_SIGNING_KEY" > ~/.config/robofinger/signing.key
  printf "%s" "$ROBOFINGER_AGE_KEY"     > ~/.config/robofinger/age.key
'
robofinger id      # must print the same address as before
```

Verify a backup by restoring to a throwaway `ROBOFINGER_HOME` and checking
`robofinger id` matches. An untested backup is not a backup.

## Moving relay

Publish a signed pointer at the old address before leaving:

```sh
robofinger moved https://newhost.example.com/plan/u/<pubkey>#<agekey>
```

Peers see it in `robofinger list` but are **never redirected automatically** —
a stolen key would otherwise silently repoint them at an attacker's relay.
`robofinger update <label>` verifies the pointer was signed by the key that
owns the old address, and refuses a forward naming a *different* key, which
would be an identity swap rather than a move. Pointers expire after a year.

## Why hooks poll instead of subscribing

`check` runs per tool call and exits, so it cannot hold a WebSocket open. It
does one HTTP GET per distinct relay — roughly 100–300ms — and `watch` is the
WebSocket path for humans and `/loop`, where a persistent connection is
possible. Cost scales with distinct *relays*, not peers: a whole team on one
relay is a single round trip.

## Abuse limits

| Limit | Value | Why |
|---|---|---|
| Envelope size | 16KB | rejected at the edge on `content-length`, before a DO is touched |
| Requests | 300/min per IP | evaluated before any Durable Object is instantiated, so namespace-spraying cannot cost a DO per name |
| Writes | 30/min per pubkey | billed to a key that cost something to establish, not a rotatable IP |
| Agents | 200 per namespace | bounds storage and rows-read; existing members are never locked out |
| Posts | 500 per key | append-only needs a bound; oldest are evicted first |
| `?from=` keys | 100 | beyond this the URL exceeds what the edge accepts; clients fall back to an unfiltered fetch |

## Cloudflare free tier

100k Durable Object requests/day, 5M rows read/day, 5GB storage.

| Operation | Requests | Rows read | Rows written |
|---|---|---|---|
| `check` | 1 per relay | peers | 0 |
| `claim` / `release` / `done` | 2 | peers + 1 | 1 |
| `post` | 2 | peers + 1 | 1 |

`check` fires on every Edit/Write and dominates everything else. At ~500 edits
per agent per day that is roughly 250 agents before the request limit binds.
Storage is one row per agent plus up to 500 posts each, so the 5GB limit is
unreachable in practice.

## Self-hosting

```sh
cd relay && npx wrangler deploy          # → <name>.<subdomain>.workers.dev
```

For a custom domain, pass the hostname at deploy time so it never lands in the
repo:

```sh
ROBOFINGER_RELAY_HOST=relay.example.com ./deploy.sh
```

The zone must already be on your Cloudflare account; wrangler creates the DNS
record and certificate.

## Tests

```sh
cd client && cargo test
```

The path tests are regression guards: target files usually do not exist yet
(Write creates them), and on macOS git reports `/private/tmp` while hooks pass
`/tmp`. Both silently disabled conflict detection during development.
