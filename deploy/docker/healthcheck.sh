#!/bin/sh
set -eu
# Loopback liveness only, no credentials. The public certificate may name a
# different host; this deliberately does not validate its trust chain.
systemctl is-active --quiet cold-vtld.service
curl --insecure --fail --silent --max-time 5 https://127.0.0.1:8443/health |
    python3 -c 'import json,sys; sys.exit(json.load(sys.stdin).get("status") != "ok")'
