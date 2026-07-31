import { DurableObject } from "cloudflare:workers";

// One Namespace DO per team. Holds the latest plan per agent and fans out
// updates to subscribers. Single-writer: an agent may only PUT its own key.
export class Namespace extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`CREATE TABLE IF NOT EXISTS plans(
      agent TEXT PRIMARY KEY,
      seq   INTEGER NOT NULL,
      epoch INTEGER NOT NULL,
      body  TEXT NOT NULL
    )`);
  }

  async fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname.replace(/^\/[^/]+\/[^/]+/, ""); // strip /ns/{name}

    if (path === "/subscribe") return this.subscribe();
    if (path === "/plans") return json(this.all());

    const put = path.match(/^\/plan\/([A-Za-z0-9._-]{1,64})$/);
    if (put && request.method === "PUT") return this.put(put[1], request);

    return new Response("not found", { status: 404 });
  }

  subscribe() {
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server);
    // Send current state immediately so a new subscriber isn't blind until the
    // next publish.
    server.send(JSON.stringify({ type: "snapshot", plans: this.all() }));
    return new Response(null, { status: 101, webSocket: client });
  }

  async put(agent, request) {
    let plan;
    try {
      plan = await request.json();
    } catch {
      return json({ error: "invalid json" }, 400);
    }
    if (plan.agent !== agent) {
      return json({ error: "agent mismatch: may only write your own key" }, 403);
    }
    const seq = Number(plan.seq);
    if (!Number.isInteger(seq) || seq < 1) {
      return json({ error: "seq must be a positive integer" }, 400);
    }

    // Monotonic seq: reject replays and out-of-order delivery.
    const [prev] = this.sql.exec("SELECT seq FROM plans WHERE agent=?", agent).toArray();
    if (prev && seq <= prev.seq) {
      return json({ error: "stale seq", have: prev.seq, got: seq }, 409);
    }

    const body = JSON.stringify(plan);
    this.sql.exec(
      `INSERT INTO plans(agent,seq,epoch,body) VALUES(?,?,?,?)
       ON CONFLICT(agent) DO UPDATE SET seq=excluded.seq, epoch=excluded.epoch, body=excluded.body`,
      agent, seq, Math.floor(Date.now() / 1000), body
    );

    this.broadcast(JSON.stringify({ type: "plan", plan }), agent);
    return json({ ok: true, seq });
  }

  all() {
    return this.sql.exec("SELECT body FROM plans").toArray()
      .map(r => JSON.parse(r.body));
  }

  broadcast(msg) {
    for (const ws of this.ctx.getWebSockets()) {
      try { ws.send(msg); } catch { /* dead socket, cleaned up on close */ }
    }
  }

  // Subscribers are read-only; ignore anything they send but keep the socket.
  async webSocketMessage(ws, msg) {
    if (msg === "ping") ws.send("pong");
  }

  async webSocketClose(ws, code, reason) {
    try { ws.close(code, reason); } catch { /* already closed */ }
  }
}

const json = (o, status = 200) =>
  new Response(JSON.stringify(o), {
    status, headers: { "content-type": "application/json" }
  });

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    // /ns/{namespace}/... — namespace is the shared secret AND the routing key.
    const m = url.pathname.match(/^\/ns\/([A-Za-z0-9._-]{8,128})(\/|$)/);
    if (!m) {
      return json({ error: "usage: /ns/{namespace}/{plan/<agent>|plans|subscribe}" }, 404);
    }
    const id = env.NAMESPACE.idFromName(m[1]);
    return env.NAMESPACE.get(id).fetch(request);
  }
};
