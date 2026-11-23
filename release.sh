#!/usr/bin/env bash
# Usage: release.sh X.Y.Z | X.Y.Z-rc.N
# Versions the workspace, finalises the changelog, commits and tags the release,
# then pushes main and the tag after confirmation on stdin. A release candidate
# (X.Y.Z-rc.N) leaves the changelog as it is: its changes stay under Unreleased
# until X.Y.Z, and the CI does not move the latest tag for it.
set -euo pipefail

die() {
  echo "release.sh: $*" >&2
  exit 1
}

version_regex='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?$'

[[ $# -eq 1 ]] || die "usage: release.sh X.Y.Z | X.Y.Z-rc.N"
version=$1
[[ "$version" =~ $version_regex ]] || die "'$version' is not of the form X.Y.Z or X.Y.Z-rc.N"
tag="v$version"

root=$(git rev-parse --show-toplevel)
cd "$root"

# --- Refusals: nothing is written before this block ends. ---
[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || die "current branch is not main"
dirty=$(git status --short --untracked-files=normal)
[[ -z "$dirty" ]] || die "working tree has modifications:"$'\n'"$dirty"
git fetch -q --no-tags origin || die "git fetch origin failed"
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || die "HEAD differs from origin/main"

git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists locally"
remote_tags=$(git ls-remote --tags origin 'refs/tags/v*') || die "cannot list tags on origin"
if grep -qxF -e "refs/tags/$tag" < <(cut -f2 <<<"$remote_tags"); then
  die "tag $tag already exists on origin"
fi

# Highest existing version, release candidates included: X.Y.Z-rc.N comes
# before X.Y.Z, as in semantic versioning (sort -V would put it after).
latest=$( {
  git tag -l 'v*'
  sed -n 's|.*refs/tags/\(v[^^]*\)$|\1|p' <<<"$remote_tags"
} | VERSION=$version python3 -c '
import os, re, sys
pattern = re.compile(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-rc\.([1-9]\d*))?")
def key(text):
    m = pattern.fullmatch(text)
    major, minor, patch, rc = m.groups()
    return (int(major), int(minor), int(patch), 0 if rc else 1, int(rc or 0))
tags = [t for t in sys.stdin.read().split() if pattern.fullmatch(t)]
if tags:
    highest = max(tags, key=key)
    if key("v" + os.environ["VERSION"]) <= key(highest):
        print(highest)
')
[[ -z "$latest" ]] || die "$version is not greater than the latest tag $latest"

python3 - <<'PY' || die "CHANGELOG.md has no non-empty '## [Unreleased]' section"
import sys
lines = open("CHANGELOG.md", encoding="utf-8").read().split("\n")
try:
    start = lines.index("## [Unreleased]")
except ValueError:
    sys.exit(1)
for line in lines[start + 1:]:
    if line.startswith("## ["):
        break
    if line.startswith("- "):
        sys.exit(0)
sys.exit(1)
PY

# --- Writes. ---
start=$(git rev-parse HEAD)
rollback() {
  trap - ERR
  git tag -d "$tag" >/dev/null 2>&1 || true
  git reset -q --hard "$start" || true
  echo "release.sh: failed, repository restored to $start" >&2
}
trap 'rollback' ERR

today=$(date +%F)
VERSION=$version TODAY=$today python3 - <<'PY'
import json
import os
import re

version = os.environ["VERSION"]
today = os.environ["TODAY"]


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def write(path, text):
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


# CHANGELOG.md: a release candidate keeps its changes under Unreleased.
if "-rc." not in version:
    text = read("CHANGELOG.md")
    text = text.replace("## [Unreleased]\n",
                        f"## [Unreleased]\n\n## [{version}] - {today}\n", 1)
    write("CHANGELOG.md", text)

# Cargo.toml: version line of [workspace.package] only
out, section, done = [], None, False
for line in read("Cargo.toml").split("\n"):
    header = re.match(r"\s*\[([^\]]+)\]\s*$", line)
    if header:
        section = header.group(1)
    elif section == "workspace.package" and not done and re.match(r"version\s*=", line):
        line = f'version = "{version}"'
        done = True
    out.append(line)
if not done:
    raise SystemExit("no version in [workspace.package]")
write("Cargo.toml", "\n".join(out))

# Cargo.lock: [[package]] blocks without a source line (workspace crates)
parts = read("Cargo.lock").split("[[package]]")
for i, block in enumerate(parts[1:], 1):
    if not re.search(r"^source\s*=", block, re.M):
        parts[i] = re.sub(r'^version = "[^"]*"', f'version = "{version}"',
                          block, count=1, flags=re.M)
write("Cargo.lock", "[[package]]".join(parts))


def rewrite_json(path, edit):
    data = json.loads(read(path))
    edit(data)
    write(path, json.dumps(data, indent=2, ensure_ascii=False) + "\n")


rewrite_json("ui/package.json", lambda d: d.update(version=version))


def lock_edit(d):
    d["version"] = version
    d["packages"][""]["version"] = version


rewrite_json("ui/package-lock.json", lock_edit)
PY

bash deploy/tests/check_release_version.sh "$tag" >/dev/null

git add CHANGELOG.md Cargo.toml Cargo.lock ui/package.json ui/package-lock.json
git commit -q -m "Release $tag"
git tag -a "$tag" -m "VaultCenter $version"
trap - ERR

echo "Commit : $(git log -1 --format='%h %s')"
echo "Tag    : $tag"
read -r -p "Push main and $tag? [y/N] " answer || answer=""
if [[ "$answer" == "y" || "$answer" == "Y" ]]; then
  if ! git push origin main; then
    echo "release.sh: push of main failed, nothing was published." >&2
    echo "To cancel: git tag -d $tag && git reset --hard HEAD~1; or retry: git push origin main && git push origin $tag" >&2
    exit 1
  fi
  if ! git push origin "$tag"; then
    echo "release.sh: main is pushed but tag $tag is not." >&2
    echo "To finish: git push origin $tag" >&2
    exit 1
  fi
else
  echo "Nothing was pushed. To cancel: git tag -d $tag && git reset --hard HEAD~1"
fi
