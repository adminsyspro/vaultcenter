#!/bin/sh
# Makes the source archive of the patched mhvtl tree, byte for byte the same
# for the same sources.
#
#   source-archive.sh <patched mhvtl git tree> <output.tar.gz>
#
# The sha256 of this archive names the DKMS version of the module (vc-<12 hex>)
# and tells the updater whether the module changed: an archive that differed
# from one image build to the next would stop every appliance and recompile
# its module at every update. So the commit that carries the patches has a
# fixed identity and a fixed date (its hash is written in the archive, and its
# date is the date of every file), and gzip records neither name nor time.
# Used by the Dockerfile and by the CI job that compiles the module, so that
# both archive the tree the same way. Run it before building anything in the
# tree: usr, include and kernel are archived as the patches left them.
set -eu

tree=$1
output=$2
stamp='2026-01-01T00:00:00+0000'

git -C "$tree" add usr include kernel
GIT_AUTHOR_NAME=VaultCenter GIT_AUTHOR_EMAIL=build@localhost GIT_AUTHOR_DATE=$stamp \
GIT_COMMITTER_NAME=VaultCenter GIT_COMMITTER_EMAIL=build@localhost GIT_COMMITTER_DATE=$stamp \
    git -C "$tree" -c commit.gpgsign=false -c core.hooksPath=/dev/null \
    commit --quiet --no-verify -m "Nonblocking status FIFO and VaultCenter telemetry"
# No pipe: a failing git archive must fail the script.
git -C "$tree" -c tar.umask=0022 archive --format=tar HEAD > "$output.tar"
gzip -n -9 < "$output.tar" > "$output"
rm -f "$output.tar"
