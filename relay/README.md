# Relay implementations

A relay is a dumb pipe. It stores signed, encrypted envelopes and hands them
back — it cannot read plans, and it decides nothing about who may read whom.
Anything that satisfies the contract below is a valid relay, so this directory
has one subdirectory per platform.

| Directory | Storage | Notes |
|---|---|---|
| [`cloudflare-d1/`](cloudflare-d1/) | D1 (SQLite) | **recommended**; free tier, quota independent of Durable Objects |
| [`cloudflare-do/`](cloudflare-do/) | Durable Objects | original; serialises writes for free, but shares an account-wide duration budget with every other DO Worker |

## The contract

All paths are relative to the relay's base URL. Usually that is just a
hostname. If the relay is hosted under a path, that path becomes the namespace:
`example.com/plan` coexists with the rest of a site, and
`example.com/plan/team-a` is a separate room with separate storage.

| Method | Path | Does |
|---|---|---|
| GET | `/plans?from=<keys>` | current claims for the listed keys |
| GET | `/posts?from=<keys>&limit=N&before=<id>` | newest-first posts |
| GET | `/u/<pubkey>` | one identity: plans, posts and forward together |
| GET | `/forward/<pubkey>` | a signed "I moved" pointer, if any |
| PUT | `/plan/<pubkey>` | publish a claim |
| PUT | `/post/<pubkey>` | append a post |
| PUT | `/forward/<pubkey>` | publish a forwarding pointer |

An implementation MUST:

- **Verify every write.** Ed25519 over `pubkey|seq|body`; reject 401 otherwise.
- **Enforce single-writer.** `PUT /plan/<k>` only accepts an envelope whose
  `pubkey` is `<k>`; reject 403 otherwise.
- **Enforce monotonic seq.** A write with `seq <= stored` is rejected 409. This
  is what stops replay, and it must hold under concurrent writes — a
  read-then-write without serialisation is a bug.
- **Filter `?from=` server-side**, so a client's read cost grows with its peer
  count rather than with everyone in the namespace.
- **Never decrypt.** `body` is opaque; it is an age ciphertext.

It SHOULD bound abuse: envelope size, writes per key per minute, distinct keys
per namespace, and posts retained per key. See either implementation for the
values in use.

See [docs/REFERENCE.md](../docs/REFERENCE.md) for the wire format and security
model.

## Adding a platform

The logic is small — roughly 500 lines, mostly signature checks and SQL. The
Cloudflare-specific parts are the storage binding and the edge rate limiter;
`crypto.subtle` is standard Web Crypto and works on Deno, Bun and Node 20+.

Copy `cloudflare-d1/` as a starting point, swap the storage calls, and check it
against the contract above. The client needs no changes — it only speaks HTTP.
