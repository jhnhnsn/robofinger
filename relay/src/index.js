import { DurableObject } from "cloudflare:workers";

// Cap on `?from=` keys per query. Bounds the SQL parameter count and stops a
// caller turning one request into an arbitrarily large read.
//
// A 43-char key plus separator is ~44 bytes, and the edge rejects URLs beyond
// roughly 4-6KB, so ~100 keys is the real ceiling regardless of what we set
// here. Clients with more peers than this should fall back to an unfiltered
// fetch rather than build a URL that 500s.
const MAX_FROM = 100;

// A plan envelope is a few hundred bytes; 16KB is generous for a large
// `touching` list encrypted to many recipients, and far below anything worth
// storing on a free relay.
const MAX_BODY = 16 * 1024;

// Distinct publishers per namespace. Bounds storage and rows-read for any one
// namespace, and a team past this size is not using it as a team any more.
const MAX_AGENTS_PER_NS = 200;

// Writes per publisher per minute. A publish happens on task boundaries, not
// per keystroke, so single digits is normal and 30 is a wide margin.
const WRITES_PER_MIN = 30;

// Posts are append-only, so unlike plans they need their own bound. 500 entries
// at ~1KB is well under any storage concern while still being a real log.
const MAX_POSTS_PER_KEY = 500;
const DEFAULT_POST_LIMIT = 20;
const MAX_POST_LIMIT = 100;

// A forward must outlive the move that prompted it, but not forever — a year
// is long enough that anyone still checking will find the trail.
const FORWARD_TTL = 365 * 24 * 60 * 60;

// One Namespace DO per relay-routing key. Holds the latest signed envelope per
// identity and fans it out to subscribers.
//
// The identity IS the Ed25519 public key — not a nickname. The relay verifies
// every write against that key, so only the holder of the matching private key
// can publish under it. There is no name to squat on.
//
// The relay never sees plan contents: `body` is age ciphertext. It reads only
// the cleartext envelope fields it needs (key, seq) to enforce ordering.
export class Namespace extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`CREATE TABLE IF NOT EXISTS plans(
      pubkey TEXT PRIMARY KEY,
      seq    INTEGER NOT NULL,
      epoch  INTEGER NOT NULL,
      body   TEXT NOT NULL
    )`);
    // Fixed-size sliding window per publisher. One row per key, overwritten —
    // never grows beyond the plans table.
    this.sql.exec(`CREATE TABLE IF NOT EXISTS writes(
      pubkey       TEXT PRIMARY KEY,
      window_start INTEGER NOT NULL,
      count        INTEGER NOT NULL
    )`);
    // Append-only posts. Deliberately a separate table from `plans`: a claim is
    // ephemeral state that expires and is overwritten, a post is a durable
    // event. Conflating them makes the log fill with routine hook-generated
    // claims and buries anything a human wrote.
    this.sql.exec(`CREATE TABLE IF NOT EXISTS posts(
      id     INTEGER PRIMARY KEY AUTOINCREMENT,
      pubkey TEXT NOT NULL,
      seq    INTEGER NOT NULL,
      epoch  INTEGER NOT NULL,
      body   TEXT NOT NULL,
      UNIQUE(pubkey, seq)
    )`);
    this.sql.exec("CREATE INDEX IF NOT EXISTS idx_posts_pubkey ON posts(pubkey, id DESC)");
    // Forwarding pointers: one per key, overwritten, long-lived.
    this.sql.exec(`CREATE TABLE IF NOT EXISTS forwards(
      pubkey TEXT PRIMARY KEY,
      seq    INTEGER NOT NULL,
      epoch  INTEGER NOT NULL,
      body   TEXT NOT NULL
    )`);
    // Pre-crypto namespaces keyed rows on `agent` and stored plaintext plans.
    // Those rows can never satisfy a signature check, so drop them rather than
    // let them surface as unverifiable garbage.
    try {
      const cols = this.sql.exec("PRAGMA table_info(plans)").toArray();
      if (!cols.some(c => c.name === "pubkey")) {
        this.sql.exec("DROP TABLE plans");
        this.sql.exec(`CREATE TABLE plans(
          pubkey TEXT PRIMARY KEY,
          seq    INTEGER NOT NULL,
          epoch  INTEGER NOT NULL,
          body   TEXT NOT NULL
        )`);
      }
    } catch { /* fresh namespace, nothing to migrate */ }
  }

  async fetch(request) {
    const url = new URL(request.url);
    const verb = request.headers.get("x-rf-verb");
    const key = request.headers.get("x-rf-key");

    if (verb === "subscribe") return this.subscribe(url);
    if (verb === "plans") return json(this.all(url.searchParams.get("from")));
    if (verb === "posts") {
      return json(this.posts(
        url.searchParams.get("from"),
        Number(url.searchParams.get("limit")) || DEFAULT_POST_LIMIT,
        Number(url.searchParams.get("before")) || null
      ));
    }

    // One identity: their plans, posts and forward in a single round trip.
    // This is the address you paste, so it should answer everything at once.
    if (verb === "u" && request.method === "GET") {
      return json({
        pubkey: key,
        plans: this.all(key),
        posts: this.posts(key, DEFAULT_POST_LIMIT, null),
        forward: this.forward(key),
      });
    }

    if (verb === "plan" && request.method === "PUT") return this.put(key, request);
    if (verb === "post" && request.method === "PUT") return this.addPost(key, request);
    if (verb === "forward" && request.method === "PUT") return this.setForward(key, request);
    if (verb === "forward" && request.method === "GET") return json(this.forward(key) ?? {});

    return new Response("not found", { status: 404 });
  }

  /// A signed "I moved" pointer. Separate from plans because it must outlive
  /// them: a peer checking back months later still needs the trail. Signed by
  /// the same key that owns the old address, so only its holder can redirect
  /// it — an unsigned redirect would be a hijacking primitive.
  async setForward(pubkey, request) {
    const { env, seq, error } = await this.readEnvelope(pubkey, request);
    if (error) return error;

    const limited = this.rateLimit(pubkey);
    if (limited) return limited;

    const [prev] = this.sql.exec(
      "SELECT seq FROM forwards WHERE pubkey=?", pubkey
    ).toArray();
    if (prev && seq <= prev.seq) {
      return json({ error: "stale seq", have: prev.seq, got: seq }, 409);
    }

    this.sql.exec(
      `INSERT INTO forwards(pubkey,seq,epoch,body) VALUES(?,?,?,?)
       ON CONFLICT(pubkey) DO UPDATE SET seq=excluded.seq, epoch=excluded.epoch, body=excluded.body`,
      pubkey, seq, Math.floor(Date.now() / 1000), JSON.stringify(env)
    );
    return json({ ok: true, seq });
  }

  /// The forward for one key, or null. Expires after FORWARD_TTL so a free
  /// relay does not carry abandoned pointers forever.
  forward(pubkey) {
    if (!pubkey) return null;
    const [row] = this.sql.exec(
      "SELECT epoch, body FROM forwards WHERE pubkey=?", pubkey
    ).toArray();
    if (!row) return null;
    if (Math.floor(Date.now() / 1000) - row.epoch > FORWARD_TTL) {
      this.sql.exec("DELETE FROM forwards WHERE pubkey=?", pubkey);
      return null;
    }
    try { return JSON.parse(row.body); } catch { return null; }
  }

  subscribe(url) {
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server);
    const from = url.searchParams.get("from");
    // Remember the filter across hibernation so pushes stay scoped.
    server.serializeAttachment({ from });
    server.send(JSON.stringify({ type: "snapshot", plans: this.all(from) }));
    return new Response(null, { status: 101, webSocket: client });
  }

  /// Read, size-check, parse and signature-verify an incoming envelope.
  /// Returns `{ env, seq }` on success or `{ error }` (a Response) on failure.
  /// Shared by `put` and `addPost` so both enforce identical rules.
  async readEnvelope(pubkey, request) {
    // Reject oversized writes on the declared length, before reading the body.
    // A plan is a few hundred bytes; anything near the cap is abuse, and we do
    // not want to buy the bandwidth to find out.
    const declared = Number(request.headers.get("content-length"));
    if (Number.isFinite(declared) && declared > MAX_BODY) {
      return { error: json({ error: `envelope too large (max ${MAX_BODY} bytes)` }, 413) };
    }

    let raw;
    try {
      raw = await request.text();
    } catch {
      return { error: json({ error: "could not read body" }, 400) };
    }
    // Chunked uploads can omit content-length, so check the real size too.
    if (raw.length > MAX_BODY) {
      return { error: json({ error: `envelope too large (max ${MAX_BODY} bytes)` }, 413) };
    }

    let env;
    try {
      env = JSON.parse(raw);
    } catch {
      return { error: json({ error: "invalid json" }, 400) };
    }
    // JSON `null` and arrays parse fine but are not envelopes.
    if (env === null || typeof env !== "object" || Array.isArray(env)) {
      return { error: json({ error: "envelope must be an object" }, 400) };
    }
    if (env.pubkey !== pubkey) {
      return { error: json({ error: "pubkey mismatch: may only write your own key" }, 403) };
    }
    const seq = Number(env.seq);
    if (!Number.isInteger(seq) || seq < 1) {
      return { error: json({ error: "seq must be a positive integer" }, 400) };
    }
    if (typeof env.body !== "string" || typeof env.sig !== "string") {
      return { error: json({ error: "body and sig required" }, 400) };
    }

    // Signature covers pubkey|seq|body, so neither the ordering nor the
    // ciphertext can be altered by the relay or anyone in between.
    const ok = await verify(pubkey, env.sig, `${pubkey}|${seq}|${env.body}`);
    if (!ok) return { error: json({ error: "bad signature" }, 401) };

    return { env, seq };
  }

  /// Sliding-window write limit per publisher. Returns a Response when the
  /// caller is over budget, otherwise null.
  rateLimit(pubkey) {
    const now = Math.floor(Date.now() / 1000);
    const [rl] = this.sql.exec(
      "SELECT window_start, count FROM writes WHERE pubkey=?", pubkey
    ).toArray();
    if (rl && now - rl.window_start < 60) {
      if (rl.count >= WRITES_PER_MIN) {
        return json(
          { error: "rate limit exceeded", retry_after: 60 - (now - rl.window_start) },
          429
        );
      }
      this.sql.exec("UPDATE writes SET count = count + 1 WHERE pubkey=?", pubkey);
    } else {
      this.sql.exec(
        `INSERT INTO writes(pubkey,window_start,count) VALUES(?,?,1)
         ON CONFLICT(pubkey) DO UPDATE SET window_start=excluded.window_start, count=1`,
        pubkey, now
      );
    }
    return null;
  }

  /// Append a post. Posts have their own seq space per publisher so writing a
  /// post never disturbs claim ordering, and vice versa.
  async addPost(pubkey, request) {
    const { env, seq, error } = await this.readEnvelope(pubkey, request);
    if (error) return error;

    const limited = this.rateLimit(pubkey);
    if (limited) return limited;

    const [prev] = this.sql.exec(
      "SELECT MAX(seq) AS seq FROM posts WHERE pubkey=?", pubkey
    ).toArray();
    if (prev?.seq != null && seq <= prev.seq) {
      return json({ error: "stale seq", have: prev.seq, got: seq }, 409);
    }

    // Bound the log per publisher so an append-only table cannot grow without
    // limit on a free relay. Oldest posts fall off first.
    const [{ n }] = this.sql.exec(
      "SELECT COUNT(*) AS n FROM posts WHERE pubkey=?", pubkey
    ).toArray();
    if (n >= MAX_POSTS_PER_KEY) {
      this.sql.exec(
        `DELETE FROM posts WHERE id IN (
           SELECT id FROM posts WHERE pubkey=? ORDER BY id ASC LIMIT ?)`,
        pubkey, n - MAX_POSTS_PER_KEY + 1
      );
    }

    this.sql.exec(
      "INSERT INTO posts(pubkey,seq,epoch,body) VALUES(?,?,?,?)",
      pubkey, seq, Math.floor(Date.now() / 1000), JSON.stringify(env)
    );

    this.broadcast(env, "post");
    return json({ ok: true, seq });
  }

  /// Newest-first posts, optionally filtered to trusted keys and paginated.
  posts(from, limit, before) {
    const want = from === null || from === undefined
      ? null
      : from.split(",").filter(Boolean).slice(0, MAX_FROM);
    if (want && want.length === 0) return [];

    limit = Math.min(Math.max(1, limit), MAX_POST_LIMIT);
    const clauses = [];
    const args = [];
    if (want) {
      clauses.push(`pubkey IN (${want.map(() => "?").join(",")})`);
      args.push(...want);
    }
    if (before) {
      clauses.push("id < ?");
      args.push(before);
    }
    const where = clauses.length ? `WHERE ${clauses.join(" AND ")}` : "";
    args.push(limit);

    return this.sql
      .exec(`SELECT id, body FROM posts ${where} ORDER BY id DESC LIMIT ?`, ...args)
      .toArray()
      .map(r => {
        try {
          return { id: r.id, ...JSON.parse(r.body) };
        } catch {
          return null;
        }
      })
      .filter(Boolean);
  }

  async put(pubkey, request) {
    const { env, seq, error } = await this.readEnvelope(pubkey, request);
    if (error) return error;

    // Monotonic seq: reject replays and out-of-order delivery.
    const [prev] = this.sql.exec("SELECT seq FROM plans WHERE pubkey=?", pubkey).toArray();
    if (prev && seq <= prev.seq) {
      return json({ error: "stale seq", have: prev.seq, got: seq }, 409);
    }

    // Rate limit per publisher. Signature verification already proved they hold
    // the key, so this is billed to an identity that cost them something to
    // establish rather than to an IP they can rotate.
    const limited = this.rateLimit(pubkey);
    if (limited) return limited;

    // Cap distinct publishers per namespace. Existing publishers are always
    // allowed through so a full namespace keeps working for its members.
    if (!prev) {
      const [{ n }] = this.sql.exec("SELECT COUNT(*) AS n FROM plans").toArray();
      if (n >= MAX_AGENTS_PER_NS) {
        return json(
          { error: `namespace full (max ${MAX_AGENTS_PER_NS} agents)` },
          507
        );
      }
    }

    const row = JSON.stringify(env);
    this.sql.exec(
      `INSERT INTO plans(pubkey,seq,epoch,body) VALUES(?,?,?,?)
       ON CONFLICT(pubkey) DO UPDATE SET seq=excluded.seq, epoch=excluded.epoch, body=excluded.body`,
      pubkey, seq, Math.floor(Date.now() / 1000), row
    );

    this.broadcast(env);
    return json({ ok: true, seq });
  }

  /// `from` is an optional comma-separated allowlist of pubkeys.
  ///
  /// Filtering happens in SQL, not JS. A client only ever wants the handful of
  /// peers it trusts, so scanning every row in the namespace would make rows-read
  /// grow with total agents rather than with the caller's peer count — the
  /// difference between O(agents) and O(peers) on every single `check`.
  all(from) {
    // `from == null` means the param was absent → return everything.
    // `?from=` present but empty means "trust nobody" → return nothing.
    const want = from === null || from === undefined
      ? null
      : from.split(",").filter(Boolean).slice(0, MAX_FROM);
    if (want && want.length === 0) return [];

    const rows = want
      ? this.sql.exec(
          `SELECT body FROM plans WHERE pubkey IN (${want.map(() => "?").join(",")})`,
          ...want
        ).toArray()
      : this.sql.exec("SELECT body FROM plans").toArray();

    return rows
      .map(r => { try { return JSON.parse(r.body); } catch { return null; } })
      .filter(r => r && typeof r.pubkey === "string");
  }

  broadcast(env, kind = "plan") {
    const msg = JSON.stringify({ type: kind, plan: env });
    for (const ws of this.ctx.getWebSockets()) {
      try {
        const { from } = ws.deserializeAttachment() ?? {};
        if (from && !from.split(",").includes(env.pubkey)) continue;
        ws.send(msg);
      } catch { /* dead socket, cleaned up on close */ }
    }
  }

  async webSocketMessage(ws, msg) {
    if (msg === "ping") ws.send("pong");
  }

  async webSocketClose(ws, code, reason) {
    try { ws.close(code, reason); } catch { /* already closed */ }
  }
}

const b64u = s => {
  const b = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(b, c => c.charCodeAt(0));
};

async function verify(pubkeyB64, sigB64, message) {
  try {
    const key = await crypto.subtle.importKey(
      "raw", b64u(pubkeyB64), { name: "Ed25519" }, false, ["verify"]
    );
    return await crypto.subtle.verify(
      { name: "Ed25519" }, key, b64u(sigB64), new TextEncoder().encode(message)
    );
  } catch {
    return false;
  }
}

const json = (o, status = 200, extraHeaders = {}) =>
  new Response(JSON.stringify(o), {
    status, headers: { "content-type": "application/json", ...extraHeaders }
  });

/// Split a request path into (base, verb).
///
/// The base path IS the namespace — a relay at `example.com/plan` and one at
/// `example.com/plan/team-a` are separate rooms with separate storage, and
/// neither collides with the rest of the site. This replaces the old
/// `/ns/{name}/` prefix: one concept (a URL) instead of two (relay + namespace).
///
/// Verbs are the last one or two segments:
///   <base>/plans            <base>/posts        <base>/subscribe
///   <base>/u/<pubkey>       — read one identity
///   <base>/plan/<pubkey>    <base>/post/<pubkey>   <base>/forward/<pubkey>
const KEY = "[A-Za-z0-9_-]{43}";
function route(pathname) {
  const two = new RegExp(`^(.*)/(u|plan|post|forward)/(${KEY})$`);
  const m2 = pathname.match(two);
  if (m2) return { base: m2[1] || "/", verb: m2[2], key: m2[3] };

  const m1 = pathname.match(/^(.*)\/(plans|posts|subscribe)$/);
  if (m1) return { base: m1[1] || "/", verb: m1[2], key: null };

  return null;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const r = route(url.pathname);
    if (!r) {
      return json(
        { error: "usage: <base>/{plans|posts|subscribe} or <base>/{u|plan|post|forward}/<pubkey>" },
        404
      );
    }

    // Reject oversized writes here, on the declared length, before the body is
    // read or a Durable Object is touched.
    const declared = Number(request.headers.get("content-length"));
    if (Number.isFinite(declared) && declared > MAX_BODY) {
      return json({ error: `envelope too large (max ${MAX_BODY} bytes)` }, 413);
    }

    // Rate limit per IP at the edge, BEFORE touching a Durable Object. Without
    // this, spraying distinct namespace names would instantiate a DO per name
    // at our expense. Per-key limits inside the DO cannot help there — the cost
    // is incurred before any key is known.
    //
    // The body must be buffered first: awaiting the limiter detaches the
    // request stream, and the DO would then fail to read it.
    let forward = request;
    if (env.RL) {
      const buffered = request.method === "PUT" ? await request.arrayBuffer() : null;
      const ip = request.headers.get("cf-connecting-ip") ?? "unknown";
      const { success } = await env.RL.limit({ key: ip });
      if (!success) {
        return json({ error: "rate limit exceeded" }, 429, { "retry-after": "60" });
      }
      if (buffered !== null) {
        if (buffered.byteLength > MAX_BODY) {
          return json({ error: `envelope too large (max ${MAX_BODY} bytes)` }, 413);
        }
        forward = new Request(request, { body: buffered });
      }
    }

    // The base path is the namespace: /plan and /plan/team-a are separate DOs.
    //
    // The parsed verb/key travel as headers because the DO cannot re-parse the
    // path without knowing where the base ends. Request headers are immutable,
    // so build a fresh Request rather than mutating the incoming one.
    const headers = new Headers(forward.headers);
    headers.set("x-rf-verb", r.verb);
    if (r.key) headers.set("x-rf-key", r.key);
    const tagged = new Request(forward.url, {
      method: forward.method,
      headers,
      body: forward.method === "PUT" ? await forward.arrayBuffer() : undefined,
    });
    const id = env.NAMESPACE.idFromName(r.base);
    return env.NAMESPACE.get(id).fetch(tagged);
  }
};
