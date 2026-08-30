#!/usr/bin/env bash
# Does the instance cap in putPlan evict the right rows?
#
# The eviction is pure SQL, so it is checked against sqlite directly rather
# than by standing up a worker. Keep the DELETE here in sync with the one in
# src/index.js — this exists to pin the ranking key, which is the part that
# was wrong first time round.
#
#   ./test_trim.sh
set -euo pipefail

CAP=10
DB=$(mktemp -u).db
trap 'rm -f "$DB"' EXIT
sqlite3 "$DB" < "$(dirname "$0")/schema.sql"

# 12 retired instances that published a lot (seq 50) a long time ago, plus one
# fresh tab that has published exactly once. Ranking on seq would call the
# fresh one the least significant and evict the row that was just written;
# ranking on epoch — wall clock, comparable across instances — does not.
sqlite3 "$DB" "
WITH RECURSIVE i(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM i WHERE n<12)
INSERT INTO plans(ns,pubkey,instance,seq,epoch,body)
SELECT 'ns','pk','old-'||n, 50, 1000+n, '{}' FROM i;
INSERT INTO plans(ns,pubkey,instance,seq,epoch,body) VALUES('ns','pk','fresh',1,9999,'{}');
DELETE FROM plans WHERE ns='ns' AND pubkey='pk' AND instance NOT IN (
  SELECT instance FROM plans WHERE ns='ns' AND pubkey='pk'
  GROUP BY instance ORDER BY MAX(epoch) DESC LIMIT $CAP);
"

q() { sqlite3 "$DB" "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

[ "$(q "SELECT COUNT(DISTINCT instance) FROM plans")" = "$CAP" ] \
  || fail "expected $CAP instances kept"
echo "ok  capped at $CAP instances"

[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance='fresh')")" = 1 ] \
  || fail "evicted the instance that just wrote — ranking key is wrong"
echo "ok  fresh low-seq instance survived"

[ "$(q "SELECT EXISTS(SELECT 1 FROM plans WHERE instance IN ('old-1','old-2'))")" = 0 ] \
  || fail "least recently active instances were not evicted"
echo "ok  least recently active evicted first"

echo
echo "PASS"
