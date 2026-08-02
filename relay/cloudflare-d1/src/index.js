// robofinger relay — Cloudflare Workers + D1.
//
// Same wire protocol as the Durable Objects variant, different storage. D1 has
// its own free-tier quota, so this relay keeps working when the account's
// Durable Object duration budget is exhausted by an unrelated Worker — the
// failure that motivated writing it.
//
// The one thing a Durable Object gave for free was serialised access: the
// monotonic `seq` check is read-compare-write, and a DO is single-threaded per
// namespace so it could not interleave. D1 has no such guarantee, so the check
// is expressed as a conditional UPDATE and we trust `meta.changes` rather than
// a prior SELECT. See `put` below.

// Cap on `?from=` keys per query. Bounds the SQL parameter count and stops a
// caller turning one request into an arbitrarily large read. ~100 is also the
// practical ceiling before the URL exceeds what the edge accepts.
const MAX_FROM = 100;

// A plan envelope is a few hundred bytes; 16KB is generous and far below
// anything worth storing on a free relay.
const MAX_BODY = 16 * 1024;

// Distinct publishers per namespace. Bounds storage and rows-read for any one
// namespace, and a team past this size is not a team any more.
const MAX_AGENTS_PER_NS = 200;

// Writes per publisher per minute. A publish happens on task boundaries, not
// per keystroke, so single digits is normal and 30 is a wide margin.
const WRITES_PER_MIN = 30;

// Posts are append-only, so unlike plans they need their own bound.
const MAX_POSTS_PER_KEY = 500;
/// Instance labels are a storage key, so they need a bound like any other.
const MAX_INSTANCE = 64;
/// Claims kept per (identity, agent): the live one plus a little history.
/// Claims republish on every edit, so this is deliberately shallow — the
/// client only writes a new row when what it holds actually changes.
const MAX_PLANS_PER_INSTANCE = 3;
const DEFAULT_POST_LIMIT = 20;
const MAX_POST_LIMIT = 100;

// A forward must outlive the move that prompted it, but not forever.
const FORWARD_TTL = 365 * 24 * 60 * 60;

const KEY = "[A-Za-z0-9_-]{43}";

/// Split a request path into (namespace, verb, key).
///
/// The base path IS the namespace, so a relay at `example.com/plan` does not
/// collide with the rest of the site and `example.com/plan/team-a` is a
/// separate room.
function route(pathname) {
  const two = new RegExp(`^(.*)/(u|plan|post|forward)/(${KEY})$`);
  const m2 = pathname.match(two);
  if (m2) return { ns: m2[1] || "/", verb: m2[2], key: m2[3] };

  const m1 = pathname.match(/^(.*)\/(plans|posts)$/);
  if (m1) return { ns: m1[1] || "/", verb: m1[2], key: null };

  return null;
}

const b64u = (s) => {
  const b = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(b, (c) => c.charCodeAt(0));
};

async function verify(pubkeyB64, sigB64, message) {
  try {
    const key = await crypto.subtle.importKey(
      "raw",
      b64u(pubkeyB64),
      { name: "Ed25519" },
      false,
      ["verify"],
    );
    return await crypto.subtle.verify(
      { name: "Ed25519" },
      key,
      b64u(sigB64),
      new TextEncoder().encode(message),
    );
  } catch {
    return false;
  }
}

const json = (o, status = 200, extraHeaders = {}) =>
  new Response(JSON.stringify(o), {
    status,
    headers: { "content-type": "application/json", ...extraHeaders },
  });

/// Read, size-check, parse and signature-verify an incoming envelope.
/// Returns `{ env, seq }` or `{ error }` (a Response).
async function readEnvelope(pubkey, request) {
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
  if (raw.length > MAX_BODY) {
    return { error: json({ error: `envelope too large (max ${MAX_BODY} bytes)` }, 413) };
  }

  let env;
  try {
    env = JSON.parse(raw);
  } catch {
    return { error: json({ error: "invalid json" }, 400) };
  }
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

  // Which agent on this identity wrote it. Cleartext because it is part of
  // the storage key — an instance label inside the ciphertext could not stop
  // two agents from overwriting each other's plan.
  const instance = typeof env.instance === "string" ? env.instance : "";
  if (instance.length > MAX_INSTANCE) {
    return { error: json({ error: `instance too long (max ${MAX_INSTANCE})` }, 400) };
  }

  // Signature covers pubkey|instance|seq|body, so neither the ordering, the
  // ciphertext, nor which agent claimed it can be altered in transit. An
  // empty instance reproduces the pre-0.2 message, so single-agent clients
  // that predate the field still verify.
  const msg = instance
    ? `${pubkey}|${instance}|${seq}|${env.body}`
    : `${pubkey}|${seq}|${env.body}`;
  const ok = await verify(pubkey, env.sig, msg);
  if (!ok) return { error: json({ error: "bad signature" }, 401) };

  return { env, seq, instance };
}

/// Sliding-window write limit per publisher. Returns a Response when over
/// budget, otherwise null.
///
/// Not atomic — two simultaneous writes can both read the same count. That is
/// acceptable: this bounds sustained abuse, and being off by one occasionally
/// costs nothing. Making it exact would need a transaction on the hot path.
async function rateLimit(db, ns, pubkey) {
  const now = Math.floor(Date.now() / 1000);
  const row = await db
    .prepare("SELECT window_start, count FROM writes WHERE ns=? AND pubkey=?")
    .bind(ns, pubkey)
    .first();

  if (row && now - row.window_start < 60) {
    if (row.count >= WRITES_PER_MIN) {
      return json(
        { error: "rate limit exceeded", retry_after: 60 - (now - row.window_start) },
        429,
      );
    }
    await db
      .prepare("UPDATE writes SET count = count + 1 WHERE ns=? AND pubkey=?")
      .bind(ns, pubkey)
      .run();
  } else {
    await db
      .prepare(
        `INSERT INTO writes(ns,pubkey,window_start,count) VALUES(?,?,?,1)
         ON CONFLICT(ns,pubkey) DO UPDATE SET window_start=excluded.window_start, count=1`,
      )
      .bind(ns, pubkey, now)
      .run();
  }
  return null;
}

/// `from` is an optional comma-separated allowlist of pubkeys.
///
/// Filtering happens in SQL so rows-read grows with the caller's peer count
/// rather than with everyone in the namespace.
function fromList(from) {
  if (from === null || from === undefined) return null;
  return from.split(",").filter(Boolean).slice(0, MAX_FROM);
}

async function getPlans(db, ns, from) {
  const want = fromList(from);
  if (want && want.length === 0) return [];

  // Newest first, so a client reading `plans` sees the live claim before the
  // history behind it. All instances of a key come back together; the client
  // groups them.
  const { results } = want
    ? await db
        .prepare(
          `SELECT body FROM plans WHERE ns=? AND pubkey IN (${want.map(() => "?").join(",")})
           ORDER BY seq DESC`,
        )
        .bind(ns, ...want)
        .all()
    : await db.prepare("SELECT body FROM plans WHERE ns=? ORDER BY seq DESC").bind(ns).all();

  return (results ?? [])
    .map((r) => {
      try {
        return JSON.parse(r.body);
      } catch {
        return null;
      }
    })
    .filter((r) => r && typeof r.pubkey === "string");
}

async function getPosts(db, ns, from, limit, before) {
  const want = fromList(from);
  if (want && want.length === 0) return [];

  limit = Math.min(Math.max(1, limit), MAX_POST_LIMIT);
  const clauses = ["ns=?"];
  const args = [ns];
  if (want) {
    clauses.push(`pubkey IN (${want.map(() => "?").join(",")})`);
    args.push(...want);
  }
  if (before) {
    clauses.push("id < ?");
    args.push(before);
  }
  args.push(limit);

  const { results } = await db
    .prepare(
      `SELECT id, body FROM posts WHERE ${clauses.join(" AND ")} ORDER BY id DESC LIMIT ?`,
    )
    .bind(...args)
    .all();

  return (results ?? [])
    .map((r) => {
      try {
        return { id: r.id, ...JSON.parse(r.body) };
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

/// A signed "I moved" pointer, or null. Expires after FORWARD_TTL so an
/// abandoned pointer does not sit on a free relay forever.
async function getForward(db, ns, pubkey) {
  if (!pubkey) return null;
  const row = await db
    .prepare("SELECT epoch, body FROM forwards WHERE ns=? AND pubkey=?")
    .bind(ns, pubkey)
    .first();
  if (!row) return null;
  if (Math.floor(Date.now() / 1000) - row.epoch > FORWARD_TTL) {
    await db.prepare("DELETE FROM forwards WHERE ns=? AND pubkey=?").bind(ns, pubkey).run();
    return null;
  }
  try {
    return JSON.parse(row.body);
  } catch {
    return null;
  }
}

/// Publish a claim.
///
/// The monotonic seq check is a conditional UPDATE rather than SELECT-then-
/// UPDATE: D1 gives no serialisation between concurrent requests, so a prior
/// read could be stale by the time we write. `meta.changes === 0` means either
/// the row does not exist yet (try the INSERT) or the stored seq was already
/// >= ours (reject).
async function putPlan(db, ns, pubkey, request) {
  const { env, seq, instance, error } = await readEnvelope(pubkey, request);
  if (error) return error;

  const limited = await rateLimit(db, ns, pubkey);
  if (limited) return limited;

  const now = Math.floor(Date.now() / 1000);
  const row = JSON.stringify(env);

  // Monotonic seq per (identity, agent). Two agents on one identity keep
  // independent counters, so neither one's writes look stale to the other.
  const cur = await db
    .prepare("SELECT MAX(seq) AS seq FROM plans WHERE ns=? AND pubkey=? AND instance=?")
    .bind(ns, pubkey, instance)
    .first();
  if (cur && cur.seq !== null && cur.seq >= seq) {
    return json({ error: "stale seq", have: cur.seq, got: seq }, 409);
  }

  // Cap distinct identities per namespace, counting people rather than
  // agents: every instance of one identity is still one publisher. Only new
  // publishers are counted, so a full namespace keeps working for its
  // existing members.
  if (!cur || cur.seq === null) {
    const { n } = await db
      .prepare("SELECT COUNT(DISTINCT pubkey) AS n FROM plans WHERE ns=?")
      .bind(ns)
      .first();
    if (n >= MAX_AGENTS_PER_NS) {
      return json({ error: `namespace full (max ${MAX_AGENTS_PER_NS} agents)` }, 507);
    }
  }

  try {
    await db
      .prepare("INSERT INTO plans(ns,pubkey,instance,seq,epoch,body) VALUES(?,?,?,?,?,?)")
      .bind(ns, pubkey, instance, seq, now, row)
      .run();
  } catch {
    // UNIQUE(ns,pubkey,instance,seq) rejected it: another write with this
    // exact seq landed first, so ours is by definition not newer. This is
    // what makes the read-then-insert above safe without a transaction.
    return json({ error: "stale seq", got: seq }, 409);
  }

  // Keep the live claim plus a little history; drop the rest.
  await db
    .prepare(
      `DELETE FROM plans WHERE ns=? AND pubkey=? AND instance=? AND seq <= (
         SELECT MIN(seq) FROM (
           SELECT seq FROM plans WHERE ns=? AND pubkey=? AND instance=?
           ORDER BY seq DESC LIMIT ?
         )
       ) - 1`,
    )
    .bind(ns, pubkey, instance, ns, pubkey, instance, MAX_PLANS_PER_INSTANCE)
    .run();

  return json({ ok: true, seq });
}

/// Append a post. Posts carry their own seq space, so posting never disturbs
/// claim ordering.
async function addPost(db, ns, pubkey, request) {
  const { env, seq, error } = await readEnvelope(pubkey, request);
  if (error) return error;

  const limited = await rateLimit(db, ns, pubkey);
  if (limited) return limited;

  const prev = await db
    .prepare("SELECT MAX(seq) AS seq FROM posts WHERE ns=? AND pubkey=?")
    .bind(ns, pubkey)
    .first();
  if (prev?.seq != null && seq <= prev.seq) {
    return json({ error: "stale seq", have: prev.seq, got: seq }, 409);
  }

  // Bound the log per publisher; oldest fall off first.
  const { n } = await db
    .prepare("SELECT COUNT(*) AS n FROM posts WHERE ns=? AND pubkey=?")
    .bind(ns, pubkey)
    .first();
  if (n >= MAX_POSTS_PER_KEY) {
    await db
      .prepare(
        `DELETE FROM posts WHERE id IN (
           SELECT id FROM posts WHERE ns=? AND pubkey=? ORDER BY id ASC LIMIT ?)`,
      )
      .bind(ns, pubkey, n - MAX_POSTS_PER_KEY + 1)
      .run();
  }

  try {
    await db
      .prepare("INSERT INTO posts(ns,pubkey,seq,epoch,body) VALUES(?,?,?,?,?)")
      .bind(ns, pubkey, seq, Math.floor(Date.now() / 1000), JSON.stringify(env))
      .run();
  } catch {
    return json({ error: "stale seq", got: seq }, 409);
  }
  return json({ ok: true, seq });
}

/// A forwarding pointer, signed by the key that owns the old address. An
/// unsigned redirect would be a hijacking primitive, so this reuses the same
/// envelope verification as everything else.
async function setForward(db, ns, pubkey, request) {
  const { env, seq, error } = await readEnvelope(pubkey, request);
  if (error) return error;

  const limited = await rateLimit(db, ns, pubkey);
  if (limited) return limited;

  const now = Math.floor(Date.now() / 1000);
  const row = JSON.stringify(env);

  const upd = await db
    .prepare("UPDATE forwards SET seq=?, epoch=?, body=? WHERE ns=? AND pubkey=? AND seq < ?")
    .bind(seq, now, row, ns, pubkey, seq)
    .run();
  if (upd.meta.changes > 0) return json({ ok: true, seq });

  const existing = await db
    .prepare("SELECT seq FROM forwards WHERE ns=? AND pubkey=?")
    .bind(ns, pubkey)
    .first();
  if (existing) {
    return json({ error: "stale seq", have: existing.seq, got: seq }, 409);
  }

  try {
    await db
      .prepare("INSERT INTO forwards(ns,pubkey,seq,epoch,body) VALUES(?,?,?,?,?)")
      .bind(ns, pubkey, seq, now, row)
      .run();
  } catch {
    return json({ error: "stale seq", got: seq }, 409);
  }
  return json({ ok: true, seq });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const r = route(url.pathname);
    if (!r) {
      // Say nothing. A relay is a dumb pipe with no public surface to browse,
      // and advertising the endpoints to anyone who wanders past only helps
      // someone deciding whether it is worth probing. Clients are built
      // against the documented contract, not against this response.
      return new Response(null, { status: 404 });
    }

    // Reject oversized writes on the declared length, before reading the body.
    const declared = Number(request.headers.get("content-length"));
    if (Number.isFinite(declared) && declared > MAX_BODY) {
      return json({ error: `envelope too large (max ${MAX_BODY} bytes)` }, 413);
    }

    // Edge rate limit, keyed by IP. Evaluated before any database work so
    // spraying namespaces cannot cost us a query per sprayed name.
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
        forward = new Request(request.url, {
          method: request.method,
          headers: request.headers,
          body: buffered,
        });
      }
    }

    const db = env.DB;
    const { ns, verb, key } = r;

    try {
      if (verb === "plans") {
        return json(await getPlans(db, ns, url.searchParams.get("from")));
      }
      if (verb === "posts") {
        return json(
          await getPosts(
            db,
            ns,
            url.searchParams.get("from"),
            Number(url.searchParams.get("limit")) || DEFAULT_POST_LIMIT,
            Number(url.searchParams.get("before")) || null,
          ),
        );
      }
      // One identity: plans, posts and forward in a single round trip, since
      // this is the address people paste.
      if (verb === "u" && request.method === "GET") {
        return json({
          pubkey: key,
          plans: await getPlans(db, ns, key),
          posts: await getPosts(db, ns, key, DEFAULT_POST_LIMIT, null),
          forward: await getForward(db, ns, key),
        });
      }
      if (verb === "plan" && request.method === "PUT") return putPlan(db, ns, key, forward);
      if (verb === "post" && request.method === "PUT") return addPost(db, ns, key, forward);
      if (verb === "forward" && request.method === "PUT") {
        return setForward(db, ns, key, forward);
      }
      if (verb === "forward" && request.method === "GET") {
        return json((await getForward(db, ns, key)) ?? {});
      }
    } catch (e) {
      return json({ error: "storage error", detail: String(e).slice(0, 200) }, 500);
    }

    return new Response("not found", { status: 404 });
  },
};
