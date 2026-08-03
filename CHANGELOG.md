# Changelog

All notable changes to robofinger. Versions follow [semver](https://semver.org),
loosely — this is pre-1.0 software and the wire format is still settling.

## v0.2.0 — 2026-08-03

Two coding agents in the same repo can now work without clobbering each other,
and a claim finally says how long it has been held.

### Several agents, one identity

Two Claudes in one repo shared a keypair, so their claims overwrote each other
— and `check` skipped its own key, so neither could see the other. The exact
failure this tool exists to prevent, happening silently.

Give each agent a name and they coexist:

```sh
ROBOFINGER_INSTANCE=claude-1 claude    # one terminal
ROBOFINGER_INSTANCE=claude-2 claude    # another
```

They stay one identity to your peers, because they are one person. Leaving
`ROBOFINGER_INSTANCE` unset is the single-agent case and changes nothing.

The name rides **outside** the encryption — the relay has to tell your agents
apart, so anyone following you sees how many you run and what you called them.
Don't use folder names if your directory layout is private.

### Claims say more

- `claimed 20m ago (idle 8m)` — a working agent republishes on every edit, so
  the old timestamp only ever showed the last refresh. `claimed_at` now
  survives republishing, which is what makes "still holding this, but quiet for
  a while" visible.
- Released, expired, and finished are now distinguishable. Letting go on
  purpose means the files are safe to pick up; a claim that rotted because the
  session died means no such thing, and the two used to render identically.
- The last 2 claims per agent are kept, so you can see what it was doing before.
- `claim` reports what it dropped — a claim replaces your list rather than
  adding to it, which used to happen silently.
- Plan lines use the post format (`<stamp> <alias>`), and timestamps are local
  time rather than UTC.

### Fixed

- **Posts silently failed** when any peer had posted more recently than you.
  `post` asked for the newest post across all peers, then filtered it for its
  own key — finding nothing, it reset `seq` to 1 and the relay correctly
  rejected the write. Present since posts were added; it needed a second
  machine to show up.
- **`robofinger check <path>` hung forever** at a terminal. It read stdin to
  EOF before looking at argv, so it waited on a tty nobody was going to close.
  Hooks pipe JSON and close, which is why it survived this long.
- Hooks now install per-repo by default (`--user` for machine-wide). Account
  scope meant every repo on the machine published claims, including ones where
  nobody else works.
- The relay root returns an empty 404 instead of advertising its endpoints.

### Upgrading

`robofinger --upgrade`.

**Self-hosting a relay?** The `plans` table changed shape and needs recreating
— see [relay/cloudflare-d1/schema.sql](relay/cloudflare-d1/schema.sql). Posts
and forwards are unaffected; claims are ephemeral, so nothing durable is lost.

A v0.1.x client still writes to a v0.2 relay, so machines can upgrade at their
own pace. A v0.2 client against an old relay fails loudly at the first write
rather than silently misfiling claims.

## v0.1.6 — 2026-08-01

- **No namespace by default.** The relay's URL path was doing little: the
  client always requests an explicit key list and never enumerates a namespace,
  so peers spread across several namespaces just cost a request each. A
  namespace is a routing key, never privacy — anyone can query any namespace,
  and your recipients can decrypt whatever they find there.
- `init` run without arguments now asks for the relay URL, alias, and optional
  namespace instead of printing a usage error.

## v0.1.5 — 2026-08-01

- **Relay moved to D1**, with `relay/` split by platform. The Durable Objects
  version shares an account-wide duration budget with every other DO Worker on
  the account — one unrelated Worker exceeding its limits could take the relay
  down. D1's quota is independent. Same wire format, no client changes.
- **`watch` removed.** It was the only feature needing a long-lived connection,
  which is what forced the Durable Object, and it was easy to start and forget.
  Cost now scales with distinct relays rather than peers.
- `--agent` renamed to `--alias` — "agent" read as *which AI* rather than
  *which machine*. `ROBOFINGER_AGENT` still works and the wire field is
  unchanged.
- `init` says when it generates keys, uses the configured alias rather than the
  hostname, and fails loudly on `--hooks` without `--url`.
- Prompts ask on the controlling terminal rather than stdin, so `ssh host
  "robofinger init …"` and piped setup scripts still get their questions.

## v0.1.4 — 2026-07-31

- **Key permissions hardened.** Keys are created 0600 at open time rather than
  chmod-ed afterward, and one readable by group or other is refused at load —
  keys arrive by backup, `cp`, and sync tools, any of which can land 0644.
- `deploy.sh` takes the relay hostname at run time, so it never rests in the
  repo. The committed config deploys to workers.dev unmodified.
- README: address anatomy, and a pass for wording.

## v0.1.3 — 2026-07-31

- **Labels ride as URL userinfo** (`https://sam@relay…`), and a suggested label
  that would shadow an existing peer is refused rather than silently replacing
  them — with a prompt for a different name.
- Peer subcommands flattened to `add` / `rm` / `list` / `update`; `list` merges
  the peer list with live claims.
- `upgrade` became `--upgrade`. Hooks are opt-in during `init`.
- The empty state points at the key exchange instead of a lookup that cannot
  work yet.

## v0.1.2 — 2026-07-31

- **`post`, `log`, and cross-relay peers.** A plan stopped being only a claim:
  posts are append-only and durable, where claims are ephemeral state that
  expires.
- **URL addresses and path-as-namespace.** One line says where and who, with
  the age key in the fragment. Signed forwarding pointers let a peer move
  relays without losing their followers — never followed automatically, since a
  stolen key could otherwise repoint them at an attacker's relay.
- `robofinger <peer>` became the main verb; a bare invocation shows your own
  status, the way `finger` with no arguments showed the local machine.
- README rewritten around what it is for, with the detail moved to
  [docs/REFERENCE.md](docs/REFERENCE.md). MIT license added.

## v0.1.1 — 2026-07-31

- `init` offers to install the Claude Code hooks, at account or project scope.
- Install path for a then-private repo.

## v0.1.0 — 2026-07-31

First release. A Cloudflare Worker relay and a Rust client: `.plan` files for a
world with agents.

- **Signed and encrypted plans.** Ed25519 for identity, age for
  multi-recipient encryption. The relay stores opaque ciphertext, verifies
  signatures, and can never read a plan.
- **Claims and conflict detection** through Claude Code hooks — `SessionStart`,
  `SessionEnd`, and a `PreToolUse` check that warns before editing a path a
  peer has claimed.
- **Abuse limits**: envelope size, per-key write rate, agents per namespace,
  and an edge rate limit evaluated before any storage work.
- `?from=` filtering in SQL, so a client's read cost grows with its peer count
  rather than with everyone on the relay.
- cargo-dist releases and `--upgrade`.
