#!/usr/bin/env bash
# Does the write bucket in rateLimit let a swarm burst and still bound abuse?
#
# The bucket is one SQL upsert, so it is checked against sqlite directly, like
# test_trim.sh. Keep the statement here in sync with rateLimit in src/index.js.
#
# What this pins:
#   - a burst of WRITE_BURST writes in one second all land: a parent and its
#     subagents claiming and releasing together is normal, not abuse
#   - the next one is refused, so the burst is a real ceiling
#   - the bucket drains at WRITES_PER_MIN, so a sustained writer at that rate
#     is never refused — the long-run bound is what it always was
#   - an idle bucket refills completely
#
#   ./test_rate.sh
set -euo pipefail

RATE=30   # WRITES_PER_MIN
BURST=120 # WRITE_BURST
DB=$(mktemp -u).db
trap 'rm -f "$DB"' EXIT
sqlite3 "$DB" < "$(dirname "$0")/schema.sql"

fail() { echo "FAIL: $*" >&2; exit 1; }

# One write at time $1; prints the new level, or nothing when refused.
write() {
  sqlite3 "$DB" "
INSERT INTO writes(ns,pubkey,window_start,count) VALUES('ns','pk',$1,60)
ON CONFLICT(ns,pubkey) DO UPDATE SET
  count = MAX(0, count - (excluded.window_start - window_start) * $RATE) + 60,
  window_start = excluded.window_start
WHERE MAX(0, count - (excluded.window_start - window_start) * $RATE) + 60 <= $BURST * 60
RETURNING count;"
}

T=1000000
for i in $(seq 1 $BURST); do
  [ -n "$(write $T)" ] || fail "write $i of a $BURST-write burst was refused"
done
echo "ok  a burst of $BURST lands"

[ -z "$(write $T)" ] || fail "write $((BURST + 1)) in the same second was accepted"
echo "ok  the one past the burst is refused"

# Two seconds drain one write's worth at 30/min.
[ -n "$(write $((T + 2)))" ] || fail "nothing drained after 2s"
[ -z "$(write $((T + 2)))" ] || fail "drained more than one write in 2s"
echo "ok  drains at $RATE a minute"

# From full, one write every 2s (= 30/min) for ten minutes: never refused.
for i in $(seq 1 300); do
  [ -n "$(write $((T + 2 + 2 * i)))" ] || fail "a sustained writer at $RATE/min was refused at write $i"
done
echo "ok  a sustained $RATE/min is never refused"

# Idle long enough to drain completely: the full burst is back.
LATER=$((T + 100000))
for i in $(seq 1 $BURST); do
  [ -n "$(write $LATER)" ] || fail "an idle bucket did not refill (write $i)"
done
echo "ok  an idle bucket refills"

echo
echo "PASS"
