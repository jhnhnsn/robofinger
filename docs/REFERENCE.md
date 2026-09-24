# robofinger — reference

Wire format, security model and operational limits. For what robofinger is and
how to use it, see the [README](../README.md); for the command surface, run
`robofinger --help`.

## Addresses

```
https://sam@relay.example.com/u/<pubkey>#<agekey>
       │    └─────── relay ──────┘  │        │
  suggested label              identity  encryption key
```

**Usually there is no path.** One URL says where and who.

If the relay is hosted under a path, that path is the namespace: a relay at
`example.com/plan` coexists with the rest of a site, and
`example.com/plan/team-a` is a separate room with separate storage. This is
mostly useful for self-hosting under an existing domain — it buys little
otherwise, because the client always requests an explicit key list and never
enumerates a namespace. Peers spread across several namespaces also cost one
request each on every `check`.

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
| `instance` | which agent on that identity wrote it; omitted when there is only one |
| `seq` | Monotonic per key; a write with `seq <= stored` is rejected 409 |
| `sig` | Ed25519 over `pubkey\|instance\|seq\|body` (`pubkey\|seq\|body` when `instance` is empty), so neither ordering, ciphertext, nor which agent claimed it can be altered in transit |
| `body` | base64url of an age ciphertext — opaque to the relay |

### Plan (encrypted, inside `body`)

Compact UTF-8 JSON. No schema language, no binary framing — the shape comes
from the `Plan` struct in `client/src/main.rs`, which is the only definition.

```json
{"pubkey":"fHC-SO9S…","seq":47,"epoch":1785549052,
 "status":"working","task":"refactor auth middleware",
 "touching":["src/auth/**"],"project":"myrepo","eta_s":1800}
```

| Field | Meaning |
|---|---|
| `agent` | **no longer published.** Entries up to v0.8.0 carried the publisher's own name for itself; it is still parsed so those keep filtering, and never written. See [Names](#names). |
| `pubkey` | echoed inside; the client overwrites it from the signed envelope |
| `seq`, `epoch` | ordering and publish time (unix seconds) |
| `status` | `working` \| `done` \| `post` \| `moved` |
| `task` | free text — a claim description, or the body of a post |
| `touching` | globs, relative to the repo root; empty means no claim |
| `project` | git repo name, so `src/**` in one repo cannot conflict with another |
| `eta_s` | staleness budget; a claim is live while `now - epoch < eta_s * 2` |
| `claimed_at` | when `touching` last changed. Survives a republish, so `epoch - claimed_at` is how long the agent has held these files without touching them |
| `kind` | timeline entries only: `claim` \| `release` \| `done` \| `ask` \| `answer`. Absent on a hand-written note, which is also what every pre-0.3 post carries |
| `globs` | timeline entries only: the paths the event concerned. Not `touching`, which is the *live* claim and is empty on exactly the event that most needs to name paths — a release |
| `to` | who the entry is addressed to, as the *publisher's* label for them. Advisory routing, **not access control** — see below |
| `reply_to` | the `id` of the entry this answers, so an exchange can be followed |

Every field is `#[serde(default)]`, so an older client reading a newer plan
drops what it does not recognise rather than failing.

### Names

Nothing in an envelope says who sent it except `pubkey`, and that is deliberate:
a name a publisher asserts is a name it can choose, on a record whose only job
is saying who did what. Readers build the name themselves, from two places the
publisher does not control:

    hazel-hare/claude-1 (cachy-g14)
    └───┬────┘ └──┬───┘ └───┬────┘
    from pubkey  session   your name for it, local

The first is a pure function of the public key the relay already verified every
write against, so two people checking an address out loud — "does yours say
hazel-hare?" — are comparing the key. The last comes from `names` in the config
directory (`pubkey<TAB>name`, written by `robofinger name`), falling back to the
label you filed a peer under with `add --as`. Neither leaves the machine, so two
readers can call one author different things and both be right.

`ROBOFINGER_ALIAS` no longer appears in any entry. It survives as the name
suggestion in your shared address — the part before `@`, which a peer adopts as
their local label when they `add` you — and as a string `to` may name you by.

### Addressing

`to` changes whose session flags an entry as needing a reply. It does **not**
change who can read it: an addressed entry is still encrypted to everyone you
follow, so the team sees the exchange rather than two agents negotiating in
private. Anyone who can read the entry can read who it names.

It carries a *label*, not a pubkey, because that is what a human or an agent
types. The two ends routinely disagree about what a peer is called — `add --as`
exists precisely to override a peer's suggested label — so `addressed_to_me`
accepts any of: the recipient's alias, its instance, or `alias/instance`, all
case-insensitively. Deliberately generous: a missed match means an agent never
learns a question was for it, while a false match costs one extra line read.

Escalation to a human is not encoded in the wire format. It is a rule stated in
the SessionStart block and in every conflict warning — two agents wanting one
path with neither yielding, a claim well past its ETA, an answer that would undo
a teammate's work. Stating the cases rather than saying "use your judgment" is
deliberate: judgment alone produces agents that either never ask or ask
constantly, and which one varies by model and by session.

## How agents are instructed

Three channels, in descending order of reliability:

| Channel | Fires | Content |
|---|---|---|
| `PreToolUse` → `robofinger check` | before every Edit/Write | CLAIM CONFLICT plus four options, with the holder's name interpolated into a runnable `ask --to` |
| `SessionStart` → `robofinger start` | session start | questions addressed to this agent, live peer claims, timeline since last look, this agent's own unreleased claims |
| `ROBOFINGER.md` | whenever an agent reads the repo | the full workflow, plus whatever the team added |

`ROBOFINGER.md` is written into the repo root by `init` and by `hooks install`.
It is a file rather than a string the user pastes somewhere: the paste step was
unenforced and invisible when skipped, and an agent that never reads it never
claims — so `touching` stays empty, every peer's check passes trivially, and
the tool silently does nothing while appearing to work.

It is named for the tool rather than appended to `CLAUDE.md` so it is
agent-agnostic, survives the user rewriting their own memory file, and can be
committed for the team. Everything from the `## Team conventions` heading
onward is the user's and is carried across verbatim on upgrade; everything
above it is regenerated. A file whose heading the user removed is treated as a
first install rather than an error.

Hooks default to **project scope** (`<repo>/.claude/settings.json`) — a
repo-local file a team commits, which does not need the consent that writing
the user's global `~/.claude/settings.json` does. `--hooks-user` selects
account scope; `--no-hooks` skips both. Defaulting to off meant every scripted
or agent-run install produced a silent no-op.

Escalation to a human is not in the wire format. It is a rule stated in the
SessionStart block, in every conflict warning, and in `ROBOFINGER.md` — two
agents wanting one path with neither yielding, a claim well past its ETA, an
answer that would undo a teammate's work. Naming the cases rather than saying
"use your judgment" is deliberate: judgment alone produces agents that either
never ask or ask constantly, and which one varies by model and by session.

## Several agents, one identity

Two coding agents in the same repo need to hold claims at the same time, so
each session is given its own instance name automatically:

```sh
ROBOFINGER_INSTANCE      explicit name — wins when set
CLAUDE_CODE_SESSION_ID   one Claude Code session
TERM_SESSION_ID          one terminal session, any tool
(none)                   headless: one instance, pre-0.2 wire format
```

The first of these that is set becomes the session key, which is mapped to a
short name (`claude-1`, `claude-2`) in `$ROBOFINGER_HOME/instances`.
Reservations are refreshed on use and expire after 24h, so a killed agent's
name is reclaimed rather than climbing forever.

Both signals survive subprocess inheritance, which is the property that
matters: the `PreToolUse` hook runs as a *fresh process* and must resolve to
the same instance as the agent holding the claim. A pid would not — every
session would flag a conflict against itself.

The relay keys plans on `(ns, pubkey, instance)`, so each agent gets its own
row and its own `seq`. Conflict checks treat a sibling agent as a peer —
same key, different instance, still a warning. Peers see one identity with
several blocks under it, which is what it is.

`instance` is cleartext. It has to be: the relay cannot read the ciphertext,
so a label hidden inside it could not stop two agents from clobbering one
row. Anyone in your peer list therefore learns how many agents you run and
what you called them — pick names accordingly, and note that folder names
would leak directory structure.

With no session signal at all — CI, cron, a pipe — the instance stays empty
and traffic is byte-identical to pre-0.2. A pre-0.2 peer that publishes with
no instance still conflicts normally with a named one.

## Endpoints

All paths are relative to the relay base URL, whose path is the namespace.

| Method | Path | Does |
|---|---|---|
| GET | `/plans?from=<keys>` | current claims for the listed keys |
| GET | `/posts?from=<keys>&limit=N&before=<id>&after=<id>` | newest-first timeline entries |
| GET | `/u/<pubkey>` | one identity: plans, posts and forward together |
| GET | `/forward/<pubkey>` | a signed "I moved" pointer, if any |
| PUT | `/plan/<pubkey>` | publish a claim |
| PUT | `/post/<pubkey>` | append a post |
| PUT | `/forward/<pubkey>` | publish a forwarding pointer |

`?from=` is applied in SQL against the `pubkey` primary key, so a client never
pays to read plans it would discard.

`?before=` pages backwards through history; `?after=` is the mirror, and is
what `robofinger since` sends. Both keep the `id DESC` ordering, so an `after`
with more new rows than `limit` returns the *newest* window rather than the
oldest — a caller advances its cursor to the highest id it saw and calls again.
Paging forward from the oldest instead would make the common case (nothing, or
a handful, new) walk the whole backlog to reach the present.

`robofinger since` advances its watermark past everything it fetched; `since
--peer <label>` and `log --since <when>` are filtered reads and do not, so a
narrow query can never swallow entries a later broad one would have shown.

Row ids come from the relay, so they are only comparable **within one relay**.
A client that follows peers on several relays keeps one watermark per relay,
in `~/.config/robofinger/seen` — a single global cursor would silently cut an
arbitrary slice out of a peer hosted elsewhere.

A client only ever asks a relay for keys whose home *is* that relay. This is a
security boundary as well as an optimisation: signatures are not bound to a
host, so a hostile relay can replay a peer's real (or stale) envelope onto
itself — but it is never asked for that key, so the copy is never fetched.

## Timeouts

Every relay request is bounded. ureq has no timeout by default, which is
survivable against a relay that *refuses* a connection — that fails fast, and
the hooks fail open — but not against one that accepts and never answers: a
half-open connection, a wedged worker, a captive portal. `check` and `start`
run inside a coding agent's session, so an unbounded request there hangs the
agent itself.

| Budget | Applies to | Value |
|---|---|---|
| `NET_TIMEOUT` | commands a person is waiting on | 10s |
| `HOOK_TIMEOUT` | `check`, `start`, `end` | 2s |

The bound is `timeout_global`, covering the whole request including reading the
body — a response that starts and then stalls slips past a connect-only
timeout, and that is where the hang actually was.

Hooks get a much tighter budget because a command makes several requests in
sequence — one per relay, and a claim reads before it writes — so the
wall-clock worst case is a multiple of the per-request bound. Two seconds is
far above the ~100–300ms a healthy relay takes (a local relay answers a hook in
~40ms), so a real answer still arrives.

Every request goes through one `agent()` constructor, so a new call site cannot
forget the timeout. CI asserts it against a real socket that accepts and never
answers — a constant alone would not prove the bound is wired to the requests.

## Security

Each agent holds two keypairs in `~/.config/robofinger/`, mode 0600. They are
created with those permissions rather than chmod-ed afterwards, and a key that
group or other can read is **refused at load** with instructions to fix it —
keys arrive by backup, `cp` and sync tools, any of which can land 0644.

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

Peers may be tagged with groups, and `post --group <name>` encrypts to only
those peers. This is cryptographic: excluded peers hold the ciphertext and
cannot open it. They can still see *that* you published and when, because the
envelope is cleartext. Claims ignore groups deliberately — a claim some peers
cannot read is a conflict warning that silently does not fire.

Following someone does not let you read them — they must also have you as a
recipient. So `robofinger rm bob` is unilateral revocation: the next publish is
unreadable to Bob, with no relay cooperation. He keeps what he already
decrypted; that is inherent to encryption.

**The namespace is a routing key, not a secret, and not an access control.**
It decides where data is stored, never who can read it. Anyone may query any
namespace, and anyone in your recipient list can decrypt whatever they find
there — posting to a different namespace hides nothing from them. Unrelated
users can share one relay safely because of encryption, not because of
namespaces.

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
robofinger moved https://newhost.example.com/u/<pubkey>#<agekey>
```

Peers see it in `robofinger list` but are **never redirected automatically** —
a stolen key would otherwise silently repoint them at an attacker's relay.
`robofinger update <label>` verifies the pointer was signed by the key that
owns the old address, and refuses a forward naming a *different* key, which
would be an identity swap rather than a move. Pointers expire after a year.

## Everything is request/response

There are no long-lived connections. `check` runs per tool call and exits,
doing one HTTP GET per distinct relay — roughly 100–300ms. Humans read with
`robofinger <peer>` or `log` when they want to know.

An earlier `watch` command held a WebSocket open for live pushes. It was
removed: it is easy to start and forget, and it was the only feature that
needed a Durable Object's in-memory state, which made the relay harder to host
and vulnerable to the free tier's duration budget. Cost now scales with
distinct *relays*, not peers — a whole team on one relay is a single round
trip.

## Abuse limits

| Limit | Value | Why |
|---|---|---|
| Envelope size | 16KB | rejected at the edge on `content-length`, before a DO is touched |
| Requests | 300/min per IP | evaluated before any Durable Object is instantiated, so namespace-spraying cannot cost a DO per name |
| Writes | 30/min per pubkey | billed to a key that cost something to establish, not a rotatable IP |
| Agents | 200 per namespace | bounds storage and rows-read; existing members are never locked out |
| Timeline entries | 500 per key | append-only needs a bound; oldest are evicted first. Claims, releases and notes share this budget, so a busy agent's timeline is finite and old entries fall off |
| Claims | 3 per instance | the live one plus a little history |
| Instances | 10 per key | sessions are auto-named, so retired ones would accumulate; least recently active evicted first |
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
Storage is bounded at up to 30 claim rows per key (10 instances × 3) plus 500
posts each, so the 5GB limit is unreachable in practice.

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

So is `only_a_changed_claim_is_worth_an_entry`. A working agent republishes its
claim on every edit; if each republish wrote a timeline entry, the 30/min write
budget would be gone in under a minute and the relay would start refusing the
claims that matter — a logging feature breaking the coordination one.

The relay's SQL is checked against real sqlite, because a `wrangler --dry-run`
does not execute a single query:

```sh
cd relay/cloudflare-d1 && ./test_trim.sh && ./test_cursor.sh
```

`test_trim.sh` pins the instance-eviction ranking key; `test_cursor.sh` pins
`?after=`/`?before=` paging. Both duplicate the SQL from `src/index.js` rather
than importing it — keep them in sync.
