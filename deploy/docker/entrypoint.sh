#!/bin/sh
set -eu
umask 0077
python3 /usr/local/libexec/vaultcenter-preflight.py
mkdir -p /var/lib/cold-vtl/restores /opt/mhvtl /run/lock/mhvtl
python3 /usr/local/libexec/isolated-boot.py
exec /sbin/init
