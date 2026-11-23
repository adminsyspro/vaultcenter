#!/usr/bin/env bash
# Usage: check_release_version.sh [vX.Y.Z | vX.Y.Z-rc.N]
# Checks that Cargo.toml, ui/package.json and CHANGELOG.md agree, and with the
# release tag when one is given. A release candidate (X.Y.Z-rc.N) has no
# changelog section of its own: its changes stay under Unreleased until X.Y.Z.
# Prints the version on stdout when consistent.
set -euo pipefail

version_regex='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?$'

fail() {
  echo "check_release_version: $*" >&2
  exit 1
}

root=$(git rev-parse --show-toplevel)
cd "$root"

cargo_version=$(python3 -c '
import sys, tomllib
with open("Cargo.toml", "rb") as f:
    print(tomllib.load(f)["workspace"]["package"]["version"])
') || fail "cannot read [workspace.package] version from Cargo.toml"

package_version=$(python3 -c '
import json
with open("ui/package.json") as f:
    print(json.load(f)["version"])
') || fail "cannot read version from ui/package.json"

[[ "$cargo_version" =~ $version_regex ]] ||
  fail "Cargo.toml version '$cargo_version' is not X.Y.Z or X.Y.Z-rc.N"
[[ "$package_version" == "$cargo_version" ]] ||
  fail "ui/package.json version '$package_version' differs from Cargo.toml '$cargo_version'"

if [[ $# -gt 0 ]]; then
  tag=$1
  [[ "$tag" == v* && "${tag#v}" =~ $version_regex ]] ||
    fail "tag '$tag' is not of the form vX.Y.Z or vX.Y.Z-rc.N"
  [[ "${tag#v}" == "$cargo_version" ]] ||
    fail "tag '$tag' differs from Cargo.toml version '$cargo_version'"
  if [[ "$cargo_version" != *-rc.* ]]; then
    grep -q -F "## [$cargo_version]" CHANGELOG.md ||
      fail "CHANGELOG.md has no section '## [$cargo_version]'"
  fi
fi

echo "$cargo_version"
