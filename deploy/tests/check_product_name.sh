#!/usr/bin/env bash
# Fails when the former product name appears outside its historical records.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# Files that must name the former paths to detect an old installation.
DETECTORS=(
  deploy/install/install.sh
  deploy/host/update.py
  deploy/tests/test_install_sh.py
  deploy/tests/test_host_update.py
  deploy/tests/install_sandbox.py
  deploy/tests/install_sandbox
)
if git grep -n -i 'freezer' -- . \
    ':(exclude)docs/superpowers/' \
    ':(exclude)docs/briefs/' \
    ':(exclude)docs/migration-to-vaultcenter.md' \
    ':(exclude)docs/multi-vtl-validation.md' \
    ':(exclude)docs/pbs-e2e-validation.md' \
    ':(exclude)docs/telemetry-acceptance-20261002.md' \
    ':(exclude)deploy/tests/check_product_name.sh' \
    "${DETECTORS[@]/#/:(exclude)}"; then
  echo 'The former product name is still present outside historical records.' >&2
  exit 1
fi
# The detectors of an old installation must name its paths, and nothing else.
leftover=$(git grep -n -i 'freezer' -- "${DETECTORS[@]}" |
  sed -E 's/freezer-host//gI; s/freezer-appliance-1//gI; s/freezer-[a-z0-9@*.-]+//gI' |
  grep -i 'freezer' || true)
if [ -n "$leftover" ]; then
  printf '%s\n' "$leftover" >&2
  echo 'The former product name is used in a detector beyond the old paths and units.' >&2
  exit 1
fi
