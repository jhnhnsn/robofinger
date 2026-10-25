#!/usr/bin/env bash
# Does the instance cap in putPlan evict the right rows?
#
# The eviction is pure SQL, so it is checked against sqlite directly rather
# than by standing up a worker. Keep the DELETE here in sync with the one in
# src/index.js — this exists to pin the ranking key, which is the part that
# was wrong first time round, and the split between sessions and subagents,
# which is what stops a swarm evicting the session that spawned it.
#
#   ./test_trim.sh
set -euo pipefail

CAP=10      # MAX_INSTANCES_PER_KEY
SUBCAP=30   # MAX_SUBAGENTS_PER_KEY
DB=$(mktemp -u).db
trap 'rm -f "$DB"' EXIT
sqlite3 "$DB" < "$(dirname "$0")/schema.sql"

q() { sqlite3 "$DB" "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# The eviction putPlan runs after a write by an instance of class $1: "top"
# for a session, "sub" for a subagent (its name has a `/`). Each class is
# capped and ranked only against itself.
trim() {
  local like cap
  if [ "$1" = sub ]; then like="LIKE '%/%'"; cap=$SUBCAP; else like="NOT LIKE '%/%'"; cap=$CAP; fi
  q "DELETE FROM plans WHERE ns='ns' AND pubkey='pk' AND instance $like AND instance NOT IN (
       SELECT instance FROM plans WHERE ns='ns' AND pubkey='pk' AND instance $like
       GROUP BY instance ORDER BY MAX(epoch) DESC LIMIT $cap)"
}

# 12 retired instances that published a lot (seq 50) a long time ago, plus one
# fresh tab that has published exactly once. Ranking on seq would call the
# fresh one the least significant and evict the row that was just written;
# ranking on epoch — wall clock, comparable across instances — does not.
q "
WITH RECURSIVE i(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM i WHERE n<12)
INSERT INTO plans(ns,pubkey,instance,seq,epoch,body)
SELECT 'ns','pk','old-'||n, 50, 1000+n, '{}' FROM i;
INSERT INTO plans(ns,pubkey,instance,seq,epoch,body) VALUES('ns','pk','fresh',1,9999,'{}');
"
trim top

[ "$(q "SELECT COUNT(DISTINCT instance) FROM plans")" = "$CAP" ] \
  || fail "expected $CAP instances kept"
echo "ok  capped at $CAP instances"

[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance='fresh')")" = 1 ] \
  || fail "evicted the instance that just wrote — ranking key is wrong"
echo "ok  fresh low-seq instance survived"

[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance IN ('old-1','old-2'))")" = 0 ] \
  || fail "least recently active instances were not evicted"
echo "ok  least recently active evicted first"

# A session holding a claim goes quiet while its swarm works: 40 subagents,
# every one newer than it. The session's row is the oldest on the key, and a
# single shared cap would evict it first — the live claim the swarm was
# spawned to work under.
q "
INSERT INTO plans(ns,pubkey,instance,seq,epoch,body) VALUES('ns','pk','fresh/explore-0',1,10000,'{}');
WITH RECURSIVE i(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM i WHERE n<40)
INSERT INTO plans(ns,pubkey,instance,seq,epoch,body)
SELECT 'ns','pk','fresh/explore-'||n, 1, 10000+n, '{}' FROM i;
"
trim sub

[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance='fresh')")" = 1 ] \
  || fail "a swarm evicted the session that spawned it"
echo "ok  a swarm never evicts a session"

[ "$(q "SELECT COUNT(DISTINCT instance) FROM plans WHERE instance LIKE '%/%'")" = "$SUBCAP" ] \
  || fail "expected $SUBCAP subagents kept"
[ "$(q "SELECT COUNT(DISTINCT instance) FROM plans WHERE instance NOT LIKE '%/%'")" = "$CAP" ] \
  || fail "subagent writes changed the session count"
echo "ok  subagents capped at $SUBCAP on their own"

[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance IN ('fresh/explore-0','fresh/explore-10'))")" = 0 ] \
  || fail "the oldest subagents were not the ones evicted"
[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance='fresh/explore-40')")" = 1 ] \
  || fail "evicted the newest subagent"
echo "ok  oldest subagents evicted first"

echo
echo "PASS"
