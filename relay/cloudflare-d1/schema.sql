-- robofinger relay schema (D1).
--
-- Every table is keyed on (ns, pubkey): the Durable Objects variant got
-- namespace isolation for free by having one object per namespace, so here it
-- becomes an explicit column and a WHERE clause.

CREATE TABLE IF NOT EXISTS plans (
  ns     TEXT NOT NULL,
  pubkey TEXT NOT NULL,
  seq    INTEGER NOT NULL,
  epoch  INTEGER NOT NULL,
  body   TEXT NOT NULL,
  PRIMARY KEY (ns, pubkey)
);

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
