import { DurableObject } from "cloudflare:workers";

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
    const path = url.pathname.replace(/^\/[^/]+\/[^/]+/, ""); // strip /ns/{name}

    if (path === "/subscribe") return this.subscribe(url);
    if (path === "/plans") return json(this.all(url.searchParams.get("from")));

    const put = path.match(/^\/plan\/([A-Za-z0-9_-]{43})$/); // base64url ed25519 pubkey
    if (put && request.method === "PUT") return this.put(put[1], request);

    return new Response("not found", { status: 404 });
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

  async put(pubkey, request) {
    let env;
    try {
      env = await request.json();
    } catch {
      return json({ error: "invalid json" }, 400);
    }
    // JSON `null` and arrays parse fine but are not envelopes.
    if (env === null || typeof env !== "object" || Array.isArray(env)) {
      return json({ error: "envelope must be an object" }, 400);
    }
    if (env.pubkey !== pubkey) {
      return json({ error: "pubkey mismatch: may only write your own key" }, 403);
    }
    const seq = Number(env.seq);
    if (!Number.isInteger(seq) || seq < 1) {
      return json({ error: "seq must be a positive integer" }, 400);
    }
    if (typeof env.body !== "string" || typeof env.sig !== "string") {
      return json({ error: "body and sig required" }, 400);
    }

    // Signature covers pubkey|seq|body, so neither the ordering nor the
    // ciphertext can be altered by the relay or anyone in between.
    const ok = await verify(pubkey, env.sig, `${pubkey}|${seq}|${env.body}`);
    if (!ok) return json({ error: "bad signature" }, 401);

    // Monotonic seq: reject replays and out-of-order delivery.
    const [prev] = this.sql.exec("SELECT seq FROM plans WHERE pubkey=?", pubkey).toArray();
    if (prev && seq <= prev.seq) {
      return json({ error: "stale seq", have: prev.seq, got: seq }, 409);
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
  all(from) {
    const rows = this.sql.exec("SELECT body FROM plans").toArray()
      .map(r => { try { return JSON.parse(r.body); } catch { return null; } })
      .filter(r => r && typeof r.pubkey === "string");
    if (!from) return rows;
    const want = new Set(from.split(",").filter(Boolean));
    return rows.filter(r => want.has(r.pubkey));
  }

  broadcast(env) {
    const msg = JSON.stringify({ type: "plan", plan: env });
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

const json = (o, status = 200) =>
  new Response(JSON.stringify(o), {
    status, headers: { "content-type": "application/json" }
  });

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    // /ns/{namespace}/... — a routing key, not a secret. Confidentiality comes
    // from encryption; authenticity from signatures.
    const m = url.pathname.match(/^\/ns\/([A-Za-z0-9._-]{4,128})(\/|$)/);
    if (!m) {
      return json({ error: "usage: /ns/{namespace}/{plan/<pubkey>|plans|subscribe}" }, 404);
    }
    const id = env.NAMESPACE.idFromName(m[1]);
    return env.NAMESPACE.get(id).fetch(request);
  }
};
