-- robofinger relay schema (D1).
--
-- Every table is keyed on (ns, pubkey): the Durable Objects variant got
-- namespace isolation for free by having one object per namespace, so here it
-- becomes an explicit column and a WHERE clause.

-- One row per (identity, agent, seq). Two agents on one identity -- two
-- Claudes in the same repo -- must hold claims simultaneously, so `instance`
-- is part of the key rather than a label inside the ciphertext the relay
-- cannot read. Empty instance is the single-agent case and what every
-- pre-0.2 client sends.
--
-- Append-only, trimmed to the last few per (ns, pubkey, instance): the
-- current claim is the newest row, and the ones behind it are the short
-- history `robofinger` shows.
CREATE TABLE IF NOT EXISTS plans (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  ns       TEXT NOT NULL,
  pubkey   TEXT NOT NULL,
  instance TEXT NOT NULL DEFAULT '',
  seq      INTEGER NOT NULL,
  epoch    INTEGER NOT NULL,
  body     TEXT NOT NULL,
  UNIQUE (ns, pubkey, instance, seq)
);
CREATE INDEX IF NOT EXISTS idx_plans_lookup
  ON plans (ns, pubkey, instance, seq DESC);

-- Append-only. Deliberately separate from `plans`: a claim is ephemeral state
-- that expires and is overwritten, a post is a durable event.
CREATE TABLE IF NOT EXISTS posts (
  id     INTEGER PRIMARY KEY AUTOINCREMENT,
  ns     TEXT NOT NULL,
  pubkey TEXT NOT NULL,
  seq    INTEGER NOT NULL,
  epoch  INTEGER NOT NULL,
  body   TEXT NOT NULL,
  UNIQUE (ns, pubkey, seq)
);
CREATE INDEX IF NOT EXISTS idx_posts_lookup ON posts (ns, pubkey, id DESC);

-- One row per key, overwritten. Long-lived: a peer checking back months later
-- still needs the trail.
CREATE TABLE IF NOT EXISTS forwards (
  ns     TEXT NOT NULL,
  pubkey TEXT NOT NULL,
  seq    INTEGER NOT NULL,
  epoch  INTEGER NOT NULL,
  body   TEXT NOT NULL,
  PRIMARY KEY (ns, pubkey)
);

-- Fixed-size sliding window per publisher; never grows beyond `plans`.
CREATE TABLE IF NOT EXISTS writes (
  ns           TEXT NOT NULL,
  pubkey       TEXT NOT NULL,
  window_start INTEGER NOT NULL,
  count        INTEGER NOT NULL,
  PRIMARY KEY (ns, pubkey)
);
