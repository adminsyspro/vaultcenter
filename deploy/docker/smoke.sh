#!/bin/bash
# Tests the delivered API/UI and database in Docker, without a kernel module.
set -euo pipefail
image=${1:?Usage: smoke.sh IMAGE [VERSION REVISION]}
expected_version=${2:-}
expected_revision=${3:-}
work=$(mktemp -d)
name="vaultcenter-smoke-$$"
volume="${name}-state"
cleanup() {
    docker logs "$name" 2>/dev/null || true
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker volume rm "$volume" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
umask 0077
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=127.0.0.1' \
    -addext 'subjectAltName=IP:127.0.0.1' \
    -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
openssl rand -hex 24 > "$work/password"
docker volume create "$volume" >/dev/null
docker run --rm -i --cap-drop=ALL --security-opt=no-new-privileges \
    --mount "type=volume,src=$volume,dst=/var/lib/cold-vtl" \
    --entrypoint /usr/local/bin/cold-vtld "$image" \
    create-user smoke-admin --role admin < "$work/password"
docker create --name "$name" --cap-drop=ALL --security-opt=no-new-privileges \
    --no-healthcheck -p 127.0.0.1:18443:8443 \
    --mount "type=volume,src=$volume,dst=/var/lib/cold-vtl" \
    -e COLD_VTL_LISTEN=0.0.0.0:8443 \
    -e COLD_VTL_PUBLIC_URL=https://127.0.0.1:18443 \
    -e COLD_VTL_TLS_CERT=/run/cert.pem \
    -e COLD_VTL_TLS_KEY=/run/key.pem \
    -e COLD_VTL_UI_DIR=/usr/local/share/vaultcenter/ui \
    --entrypoint /usr/local/bin/cold-vtld "$image" >/dev/null
# The installer extracts it from the image: it must be the one in the repository.
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
shipped=$(docker cp "$name:/usr/local/share/vaultcenter/host/install.sh" - | tar -xO | sha256sum | cut -d' ' -f1)
expected=$(sha256sum "$repo/deploy/install/install.sh" | cut -d' ' -f1)
[ "$shipped" = "$expected" ] || { echo "install.sh in the image differs from deploy/install/install.sh" >&2; exit 1; }
# Copy as container root: runner-owned 0700/0600 bind mounts are unreadable
# with all capabilities dropped. Keep the key private and omit the password.
tar -C "$work" --owner=0 --group=0 -cf - cert.pem key.pem | docker cp - "$name:/run/"
docker start "$name" >/dev/null
python3 - "$work" "$expected_version" "$expected_revision" <<'PY'
from http.cookiejar import CookieJar
import json
from pathlib import Path
import ssl
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.request import build_opener, HTTPCookieProcessor, HTTPSHandler, Request

work = Path(sys.argv[1])
expected_version = sys.argv[2]
expected_revision = sys.argv[3]
base = 'https://127.0.0.1:18443'
opener = build_opener(HTTPSHandler(context=ssl.create_default_context(cafile=str(work / 'cert.pem'))),
                     HTTPCookieProcessor(CookieJar()))
last_error = None
for attempt in range(60):
    try:
        with opener.open(base + '/health', timeout=2) as response:
            health = json.load(response)
        assert health['status'] == 'ok', health
        break
    except (OSError, URLError) as error:
        last_error = error
        time.sleep(1)
else:
    raise AssertionError(f'API did not become healthy: {last_error!r}') from last_error
if expected_version:
    assert health.get('version') == expected_version, \
        f"/health version {health.get('version')!r} != expected {expected_version!r}"
if expected_revision:
    assert health.get('revision') == expected_revision, \
        f"/health revision {health.get('revision')!r} != expected {expected_revision!r}"
with opener.open(base) as response:
    assert b'VaultCenter' in response.read()
try:
    opener.open(base + '/v1/vtls')
except HTTPError as error:
    assert error.code == 401, error.code
else:
    raise AssertionError('Anonymous API access was allowed')
request = Request(base + '/auth/login', method='POST',
                  headers={'Content-Type': 'application/json', 'Origin': base},
                  data=json.dumps({'username': 'smoke-admin',
                                   'password': (work / 'password').read_text().strip()}).encode())
with opener.open(request) as response:
    session = json.load(response)
    assert session['user']['role'] == 'admin'
    assert session['csrf_token']
with opener.open(base + '/v1/vtls') as response:
    libraries = json.load(response)
    assert len(libraries) == 1
checked = f" Version {health['version']} (revision {health['revision']}) verified." if expected_version or expected_revision else ''
print('Docker smoke passed: TLS, UI, migrations, bootstrap, auth and VTL inventory.' + checked)
PY
