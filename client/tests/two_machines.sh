#!/usr/bin/env bash
# Does a plan written on one machine actually arrive, decryptable, on another?
#
# Two ROBOFINGER_HOME dirs are two machines as far as this code is concerned:
# separate keys, peers file and config. Everything downstream of that — signing,
# age-encrypting to the peer, the PUT, the relay, the fetch, verify, decrypt —
# is the same code path a second laptop runs. What this does NOT cover is the
# network between two hosts; that is ureq and Cloudflare, not robofinger.
#
#   ./two_machines.sh                          # against $ROBOFINGER_URL
#   ROBOFINGER_URL=https://relay.example.com ./two_machines.sh
#
# Needs a reachable relay. Writes to it under two throwaway identities.
set -euo pipefail

BIN="${ROBOFINGER_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/robofinger}"
URL="${ROBOFINGER_URL:-}"
[ -x "$BIN" ] || { echo "no binary at $BIN (cargo build --release)" >&2; exit 1; }
if [ -z "$URL" ]; then
  URL=$(sed -n 's/^ROBOFINGER_URL=//p' "${HOME}/.config/robofinger/config" 2>/dev/null | head -1)
fi
[ -n "$URL" ] || { echo "set ROBOFINGER_URL to a relay" >&2; exit 1; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Unique text per run, so a pass can never be a stale row from a previous run.
STAMP="two-machines-$$-$(date +%s)"

# `a` and `b` are the two machines. Each gets its own key dir and config.
for m in a b; do
  mkdir -p "$TMP/$m"
  printf 'ROBOFINGER_URL=%s\nROBOFINGER_ALIAS=%s\n' "$URL" "machine-$m" > "$TMP/$m/config"
done
run() { local m=$1; shift; ROBOFINGER_HOME="$TMP/$m" "$BIN" "$@"; }

fail() { echo "FAIL: $*" >&2; exit 1; }

# --- exchange addresses, both directions -------------------------------------
# Adding a peer does two jobs at once: subscribes you to them, and adds them as
# an encryption recipient. One direction alone leaves the other end unable to
# decrypt, which is the single most common way this is misconfigured.
ADDR_A=$(run a id 2>/dev/null)
ADDR_B=$(run b id 2>/dev/null)
[ -n "$ADDR_A" ] && [ -n "$ADDR_B" ] || fail "no identity blob"
run a add "$ADDR_B" --as bee  > /dev/null
run b add "$ADDR_A" --as ayy  > /dev/null

# --- a posts, b reads --------------------------------------------------------
run a post "$STAMP" > /dev/null
run b log -n 50 | grep -qF "$STAMP" || fail "b never saw a's post"
echo "ok  post crossed: a -> b"

# --- a claims, b sees the conflict ------------------------------------------
# Claims are the point of the tool, and they travel a different table than
# posts, so a working post proves nothing about them.
CLAIMED="src/crosscheck-$$.rs"
run a claim "$STAMP" "$CLAIMED" > /dev/null
run b list | grep -qF "$CLAIMED" || fail "b never saw a's claim"
echo "ok  claim crossed: a -> b"

# The conflict check is what actually fires in a hook. Same repo on both ends,
# so `project` matches — a claim from another project must not warn.
run b check "$(git rev-parse --show-toplevel)/$CLAIMED" \
  | grep -q "CLAIM CONFLICT" || fail "b's check did not flag a's claim"
echo "ok  conflict check fired on b"

# --- and back the other way --------------------------------------------------
# Asymmetric bugs are real: b may be able to read a while a cannot read b.
run b post "$STAMP-reply" > /dev/null
run a log -n 50 | grep -qF "$STAMP-reply" || fail "a never saw b's reply"
echo "ok  post crossed: b -> a"

# --- a stranger must NOT be able to read -------------------------------------
# The negative case. Without it this suite passes just as happily if encryption
# is a no-op and every body is readable by anyone who asks.
mkdir -p "$TMP/c"
printf 'ROBOFINGER_URL=%s\nROBOFINGER_ALIAS=machine-c\n' "$URL" > "$TMP/c/config"
run c add "$ADDR_A" --as ayy > /dev/null   # c follows a; a never added c
if run c log -n 50 | grep -qF "$STAMP"; then
  fail "stranger decrypted a's post — encryption is not doing its job"
fi
echo "ok  stranger cannot read (not a recipient)"

# --- the timeline: b can ask what a has done since it last looked ------------
# `list` and `check` answer "right now". This answers "while I was away", which
# is the half git cannot supply until someone commits.
run b since > /dev/null                    # consume the backlog, set a watermark
run b since | grep -q "nothing new" || fail "b's watermark did not advance"
echo "ok  since is empty once caught up"

run a claim "$STAMP-timeline" "src/tl-$$.rs" > /dev/null
run b since | grep -qF "$STAMP-timeline" || fail "b never saw a's claim entry"
echo "ok  claim reached b's timeline"

# The gate that keeps a working agent from spending its whole write budget
# journalling itself: republishing an unchanged claim is not an event.
run a claim "$STAMP-timeline" "src/tl-$$.rs" > /dev/null
run b since | grep -q "nothing new" || fail "an unchanged republish wrote an entry"
echo "ok  unchanged republish writes nothing"

# --- ask/answer: the escalation path between two robots ----------------------
# The reason the tool exists is that agents settle things between themselves.
# A question that does not reach the agent it names is the failure that matters.
run a ask --to machine-b "$STAMP-question" > /dev/null
run b since | grep -qF "$STAMP-question" || fail "b never saw a's question"
echo "ok  question reached b"

# The SessionStart hook is where an agent actually reads this, and it must mark
# a directed question as being for the recipient specifically.
run b start | grep -qF "FOR YOU" || fail "b's session start did not flag the question"
echo "ok  question marked FOR YOU at session start"

run b answer --to machine-a --re 1 "$STAMP-answer" > /dev/null
run a since | grep -qF "$STAMP-answer" || fail "a never saw b's answer"
echo "ok  answer reached a"

# --- release, so the claim does not linger on the relay ----------------------
run a release --note "$STAMP-done" > /dev/null
run b since | grep -qF "$STAMP-done" || fail "b never saw a's release note"
echo "ok  release note reached b's timeline"

run a release > /dev/null

echo
echo "PASS  $URL"
