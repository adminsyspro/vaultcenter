#!/usr/bin/env bash
# Prepares the DKMS source tree of the patched mhvtl module.
#
#   prepare-dkms-source.sh <archive> <destination-root> <dkms.conf>
#   prepare-dkms-source.sh --version <archive>
#
# Unpacks <archive> (git archive of the patched mhvtl tree) into
# <destination-root>/usr/src/mhvtl-<version>/ with <dkms.conf> written there,
# @VERSION@ replaced, and prints <version>: "vc-" followed by the first 12
# characters of the sha256 of the archive. Used by install.sh and by CI, so
# that both prepare the tree the same way. Running it again on a prepared tree
# changes nothing: the same archive always gives the same tree.
set -euo pipefail

archive_version() {
    local sum
    sum="$(sha256sum "$1")"
    printf 'vc-%s\n' "${sum:0:12}"
}

if [[ "${1:-}" == "--version" && $# -eq 2 ]]; then
    archive_version "$2"
    exit 0
fi
if [[ $# -ne 3 ]]; then
    printf 'usage: %s <archive> <destination-root> <dkms.conf>\n       %s --version <archive>\n' "$0" "$0" >&2
    exit 2
fi

archive="$1"
destination_root="${2%/}"
template="$3"
version="$(archive_version "${archive}")"
tree="${destination_root}/usr/src/mhvtl-${version}"

if [[ ! -f "${tree}/dkms.conf" ]]; then
    partial="${tree}.partial"
    rm -rf "${partial}" "${tree}"
    mkdir -p "${partial}"
    tar -xzf "${archive}" -C "${partial}"
    sed "s/@VERSION@/${version}/" "${template}" >"${partial}/dkms.conf"
    mv "${partial}" "${tree}"
fi
printf '%s\n' "${version}"
