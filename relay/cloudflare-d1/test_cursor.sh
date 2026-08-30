#!/usr/bin/env bash
# Does the `?after=` cursor in getPosts return the right window?
#
# Same approach as test_trim.sh: the paging is pure SQL, so it is checked
# against sqlite directly rather than by standing up a worker. Keep the WHERE
# clauses here in sync with getPosts in src/index.js.
#
# What this pins:
#   - `after` returns strictly newer rows, so a cursor never replays an entry
#   - `before` still means what it always did, so `log` is unaffected
#   - both together bound a window, since they are separate clauses on one query
#   - ordering stays id DESC, which is why a caller advances to the HIGHEST id
#     it saw rather than the last one printed
#
#   ./test_cursor.sh
set -euo pipefail

DB=$(mktemp -u).db
trap 'rm -f "$DB"' EXIT
sqlite3 "$DB" < "$(dirname "$0")/schema.sql"

# Ten posts from one key, ids 1..10 (AUTOINCREMENT), plus one from a second
# key so the `from=` filter has something to exclude.
sqlite3 "$DB" "
WITH RECURSIVE i(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM i WHERE n<10)
INSERT INTO posts(ns,pubkey,seq,epoch,body) SELECT 'ns','pk',n,1000+n,'{}' FROM i;
INSERT INTO posts(ns,pubkey,seq,epoch,body) VALUES('ns','other',1,2000,'{}');
"

q() { sqlite3 "$DB" "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# getPosts(ns, from=pk, limit, before, after) — the clauses it assembles.
after()  { q "SELECT id FROM posts WHERE ns='ns' AND pubkey='pk' AND id > $1 ORDER BY id DESC LIMIT $2"; }
before() { q "SELECT id FROM posts WHERE ns='ns' AND pubkey='pk' AND id < $1 ORDER BY id DESC LIMIT $2"; }

[ "$(after 7 100 | tr '\n' ',')" = "10,9,8," ] \
  || fail "after=7 should be exactly 10,9,8 — got $(after 7 100 | tr '\n' ',')"
echo "ok  after= returns strictly newer rows"

[ -z "$(after 10 100)" ] || fail "after= the newest id should return nothing"
echo "ok  a caught-up cursor sees nothing"

[ "$(after 0 100 | wc -l | tr -d ' ')" = 10 ] \
  || fail "after=0 should be the whole window"
echo "ok  a fresh cursor sees everything"

# The plan's ordering note, pinned: more new rows than `limit` returns the
# NEWEST window, not the oldest. A caller that advanced to the LOWEST id it
# saw would loop forever on the same three rows.
[ "$(after 0 3 | tr '\n' ',')" = "10,9,8," ] \
  || fail "a limited after= must return the newest window, not the oldest"
echo "ok  limit keeps the newest end of the window"

[ "$(before 4 100 | tr '\n' ',')" = "3,2,1," ] \
  || fail "before= regressed — got $(before 4 100 | tr '\n' ',')"
echo "ok  before= is unchanged"

# Both clauses on one query, which is what happens if a caller sends both.
[ "$(q "SELECT id FROM posts WHERE ns='ns' AND pubkey='pk' AND id < 8 AND id > 4 ORDER BY id DESC LIMIT 100" | tr '\n' ',')" = "7,6,5," ] \
  || fail "before= and after= together should bound a window"
echo "ok  before= and after= bound a window together"

[ "$(q "SELECT COUNT(*) FROM posts WHERE ns='ns' AND pubkey IN ('pk') AND id > 0")" = 10 ] \
  || fail "from= should exclude the other key"
echo "ok  from= still scopes to the requested keys"

echo
echo "PASS"
