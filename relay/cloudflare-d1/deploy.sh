#!/bin/sh
# Deploy the relay to a custom domain.
#
# The committed wrangler.jsonc deploys to workers.dev so anyone can clone and
# run it unmodified. Serving from your own hostname needs a value that should
# not live in a public repo, so it is supplied at deploy time:
#
#   ROBOFINGER_RELAY_HOST   e.g. relay.example.com
#   CLOUDFLARE_ACCOUNT_ID   optional; only needed with several accounts
#
# With envstow:
#
#   envstow run --store robofinger \
#     --only ROBOFINGER_RELAY_HOST,CLOUDFLARE_ACCOUNT_ID -- ./deploy.sh
#
# Or plainly:
#
#   ROBOFINGER_RELAY_HOST=relay.example.com ./deploy.sh

set -eu

cd "$(dirname "$0")"

if [ -z "${ROBOFINGER_RELAY_HOST:-}" ]; then
  echo "deploy: ROBOFINGER_RELAY_HOST is not set." >&2
  echo "  envstow run --store robofinger --only ROBOFINGER_RELAY_HOST -- ./deploy.sh" >&2
  echo "  (or just: npx wrangler deploy — publishes to workers.dev)" >&2
  exit 1
fi

# `workers_dev` and `custom_domain` are config-only — there are no CLI flags —
# so generate a config from the committed one with the hostname patched in.
#
# It must sit beside the real config: wrangler resolves the Worker name and
# `main` relative to the config file's directory, so a file in /tmp deploys as
# a Worker called "tmp" with no entry point. Gitignored and removed on exit, so
# the hostname never rests on disk.
tmp="./.wrangler-deploy.jsonc"
trap 'rm -f "$tmp"' EXIT INT TERM

python3 - "$tmp" <<'PY'
import json, os, re, sys

# The committed config is JSONC; strip // comments before parsing.
src = open("wrangler.jsonc").read()
src = re.sub(r'^\s*//.*$', '', src, flags=re.M)
cfg = json.loads(src)

cfg["workers_dev"] = False
cfg["routes"] = [{"pattern": os.environ["ROBOFINGER_RELAY_HOST"], "custom_domain": True}]

json.dump(cfg, open(sys.argv[1], "w"), indent=2)
PY

# Not `exec` — that would replace this shell and the EXIT trap would never run,
# leaving the generated config (with the hostname) sitting in the repo.
npx wrangler deploy -c "$tmp" "$@"
