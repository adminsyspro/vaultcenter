"""Shared behaviour of the install sandbox doubles.

Usage: _double.py <tool> [argv...]. Appends {"tool", "argv"[, "env"]} to calls.jsonl
and answers from scenario.json, both in $VAULTCENTER_SANDBOX_DIR. "env" holds the
RECORDED_ENV variables that are set (proxy, apt, docker client configuration).
The scenario is a list of {"tool", "argv_prefix", "exit", "stdout"} with optional
"contains" (words that must all appear in argv, anywhere) and "installs" (tools
that become available once the call succeeds, e.g. docker after apt-get install) and
"stderr" (text written on standard error) and "block" (write $VAULTCENTER_SANDBOX_DIR/blocked, then sleep: lets a test signal the script;
with "block": "signal" the call waits for SIGINT, SIGTERM or SIGHUP, survives it, and then answers as declared,
like the updater, which rolls back and reports when it is interrupted).
The most specific entry wins (longest prefix, then most words in "contains"),
the last one declared on a tie.

The tool "update.py" is the stand-in for the updater shipped in the payload
(host/update.py); it is recorded like the others and prints the scenario's JSON.

Standard input is read, and kept in stdin.jsonl (never in calls.jsonl), only for
calls that are documented to read it: curl --data @- and docker login
--password-stdin. A `docker cp <container>:<path> <dest>` that succeeds creates
the destination, so later steps find the files; curl -o writes a placeholder.

A curl call to a loopback address fails, as the real one does, when a proxy is
set in the environment and the call does not say --noproxy: the proxy cannot
reach the machine's own 127.0.0.1.
"""
import json
import os
import shutil
import signal
import sys
import time
from pathlib import Path

FINGERPRINT = ":".join(["AB", "CD", "EF", "01"] * 8)
FINGERPRINT_LINE = f"sha256 Fingerprint={FINGERPRINT}\n"
DEFAULTS = [
    {"tool": "id", "argv_prefix": ["-u"], "exit": 0, "stdout": "0\n"},
    {"tool": "uname", "argv_prefix": ["-m"], "exit": 0, "stdout": "x86_64\n"},
    {"tool": "uname", "argv_prefix": ["-r"], "exit": 0, "stdout": "6.1.0-test-amd64\n"},
    {"tool": "hostname", "argv_prefix": [], "exit": 0, "stdout": "vaultcenter-test\n"},
    {"tool": "hostname", "argv_prefix": ["-f"], "exit": 0, "stdout": "vaultcenter-test.example.test\n"},
    # One interface with an IPv4 and an IPv6 address, and a default route through it.
    {"tool": "ip", "argv_prefix": ["-4", "route", "get"], "exit": 0,
     "stdout": "1.1.1.1 via 10.0.0.1 dev eth0 src 10.0.0.5 uid 0\n    cache\n"},
    {"tool": "ip", "argv_prefix": ["-o", "addr", "show"], "exit": 0,
     "stdout": "2: eth0    inet 10.0.0.5/24 brd 10.0.0.255 scope global eth0\\       valid_lft forever preferred_lft forever\n"
               "2: eth0    inet6 2001:db8::5/64 scope global \\       valid_lft forever preferred_lft forever\n"},
    {"tool": "openssl", "argv_prefix": ["req"], "exit": 0, "stdout": ""},
    {"tool": "openssl", "argv_prefix": ["x509"], "exit": 0, "stdout": FINGERPRINT_LINE},
    # A healthy machine, for the preflight: scenarios override one fact each.
    {"tool": "systemd-detect-virt", "argv_prefix": [], "exit": 1, "stdout": "none\n"},
    {"tool": "systemctl", "argv_prefix": ["is-active"], "exit": 3, "stdout": "inactive\n"},
    {"tool": "pgrep", "argv_prefix": [], "exit": 1, "stdout": ""},
    {"tool": "mokutil", "argv_prefix": ["--sb-state"], "exit": 0, "stdout": "SecureBoot disabled\n"},
    {"tool": "apt-cache", "argv_prefix": ["policy"], "exit": 0,
     "stdout": "linux-headers-6.1.0-test-amd64:\n  Installed: (none)\n  Candidate: 6.1.0-1\n"},
    {"tool": "nproc", "argv_prefix": [], "exit": 0, "stdout": "4\n"},
    {"tool": "df", "argv_prefix": ["-Pk"], "exit": 0,
     "stdout": "Filesystem 1024-blocks Used Available Capacity Mounted on\n"
               "/dev/sda1 104857600 20971520 83886080 20% /\n"},
    {"tool": "curl", "argv_prefix": [], "exit": 0, "stdout": "200"},
    # The installation site accepts any token.
    {"tool": "curl", "argv_prefix": [], "contains": ["https://vaultcenter.io/api/v1/install/validate"],
     "exit": 0, "stdout": json.dumps({"valid": True, "registry": "ghcr.io",
                                      "username": "vc-customer", "password": "ghp_SITEPASSWORD789"})},
    # Installing docker-ce makes the docker double appear on PATH.
    {"tool": "apt-get", "argv_prefix": [], "contains": ["install", "docker-ce"], "exit": 0,
     "stdout": "", "installs": ["docker"]},
    # The registry serves one image whose version label is 0.1.0.
    {"tool": "docker", "argv_prefix": ["image", "inspect"], "contains": ["{{index .RepoDigests 0}}"],
     "exit": 0, "stdout": "ghcr.io/adminsyspro/vaultcenter@sha256:" + "0123456789abcdef" * 4 + "\n"},
    {"tool": "docker", "argv_prefix": ["image", "inspect"],
     "contains": ['{{index .Config.Labels "org.opencontainers.image.version"}}'],
     "exit": 0, "stdout": "0.1.0\n"},
    {"tool": "docker", "argv_prefix": ["cp"], "exit": 0, "stdout": ""},
    # The host installer reads the resolved compose file and two named volumes.
    {"tool": "docker", "argv_prefix": ["compose", "-f"], "contains": ["config", "json"],
     "exit": 0, "stdout": "@compose-config@"},
    {"tool": "docker", "argv_prefix": ["volume", "create"], "exit": 0, "stdout": "@volume-create@"},
    {"tool": "docker", "argv_prefix": ["volume", "inspect"], "exit": 0, "stdout": "@volume-inspect@"},
    # No appliance container before `up`, then a healthy one running the image.
    {"tool": "docker", "argv_prefix": ["inspect"], "exit": 1, "stdout": ""},
    {"tool": "docker", "argv_prefix": ["inspect", "--format", "{{.State.Health.Status}}"],
     "exit": 0, "stdout": "healthy\n"},
    {"tool": "curl", "argv_prefix": [], "contains": ["https://127.0.0.1:8443/health"],
     "exit": 0, "stdout": json.dumps({"status": "ok", "version": "0.1.0"})},
]
UPDATED_RESULT = {"result": "updated", "from": "0.0.9", "to": "0.1.0", "reasons": [], "warnings": [],
                  "backup": "/var/lib/vaultcenter-host/backups/20261113-120000-0.0.9",
                  "still_in_maintenance": [], "maintenance_left": True, "step": None, "confirmable": False}
DEFAULTS.append({"tool": "update.py", "argv_prefix": [], "exit": 0, "stdout": json.dumps(UPDATED_RESULT) + "\n"})
RECORDED_ENV = ("VAULTCENTER_INSTALL_ROOT", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "NO_PROXY",
                "DEBIAN_FRONTEND", "DOCKER_CONFIG")
STDIN_READERS = {("curl", "@-"), ("docker", "--password-stdin"), ("docker", "create-user")}


def copy_from_payload(source: Path, dest: str) -> None:
    """Copy a file or tree of the sandbox payload to where docker cp puts it."""
    target = Path(dest) / source.name if dest.endswith("/") else Path(dest)
    if source.is_dir():
        shutil.copytree(source, target, dirs_exist_ok=True)
    else:
        shutil.copy2(source, target)


def simulate_docker_cp(argv: list[str]) -> int:
    """docker cp <container>:<path> <dest>: create what the real one would copy
    (the sandbox payload when it holds that name, else a placeholder)."""
    source, dest = argv[1], argv[2]
    name = Path(source.split(":", 1)[1]).name
    payload = Path(os.environ["VAULTCENTER_SANDBOX_DIR"]) / "payload" / name
    if payload.exists():
        directory = Path(dest) if dest.endswith("/") else Path(dest).parent
        if not directory.is_dir():
            sys.stderr.write(f"Error: no such directory {directory}\n")
            return 1
        copy_from_payload(payload, dest)
        return 0
    if dest.endswith("/"):
        target = Path(dest) / name
        if not Path(dest).is_dir():
            sys.stderr.write(f"Error: no such directory {dest}\n")
            return 1
        target.mkdir(exist_ok=True)
        (target / "simulated").write_text("simulated\n")
    else:
        if not Path(dest).parent.is_dir():
            sys.stderr.write(f"Error: no such directory {Path(dest).parent}\n")
            return 1
        Path(dest).write_text("simulated\n")
    return 0


def volume_dir(name: str) -> Path:
    root = os.environ.get("VAULTCENTER_INSTALL_ROOT", "")
    return Path(root + "/var/lib/docker/volumes/" + name + "/_data")


def dynamic_output(tool: str, argv: list[str], text: str) -> str:
    """Answers that depend on the call: the compose file resolved with the image
    given in the environment, and docker volumes that live in the sandbox root."""
    if text == "@compose-config@":
        return json.dumps({
            "name": "vaultcenter",
            "services": {"appliance": {"image": os.environ.get("VAULTCENTER_IMAGE", ""), "volumes": []}},
            "volumes": {"state": {"name": "vaultcenter_state"}, "media": {"name": "vaultcenter_media"}}})
    if text == "@volume-create@":
        volume_dir(argv[2]).mkdir(parents=True, exist_ok=True)
        return argv[2] + "\n"
    if text == "@volume-inspect@":
        return json.dumps([{"Mountpoint": str(volume_dir(argv[2]))}])
    return text


def simulate_openssl_req(argv: list[str]) -> None:
    """openssl req -x509 ... -keyout K -out C: write a key and a certificate."""
    Path(argv[argv.index("-keyout") + 1]).write_text("-----BEGIN PRIVATE KEY-----\nsimulated\n")
    Path(argv[argv.index("-out") + 1]).write_text("-----BEGIN CERTIFICATE-----\nsimulated\n")


def through_a_proxy(tool: str, argv: list[str]) -> bool:
    """A curl call to the loopback that the environment sends to a proxy."""
    proxied = any(os.environ.get(name) for name in ("HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"))
    loopback = any(word.startswith(("https://127.0.0.1", "https://localhost")) for word in argv)
    return tool == "curl" and proxied and loopback and "--noproxy" not in argv


def wait_for_a_signal(state: Path, tool: str) -> None:
    """Tell the test the call started, then wait until a signal comes (and
    survive it): the answer follows."""
    received = []
    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, lambda *_: received.append(True))
    (state / "blocked").write_text(tool)
    deadline = time.monotonic() + 60
    while not received and time.monotonic() < deadline:
        time.sleep(0.02)


def main() -> int:
    tool, argv = sys.argv[1], sys.argv[2:]
    state = Path(os.environ["VAULTCENTER_SANDBOX_DIR"])
    record = {"tool": tool, "argv": argv}
    env = {name: os.environ[name] for name in RECORDED_ENV if name in os.environ}
    if env:
        record["env"] = env
    with (state / "calls.jsonl").open("a") as calls:
        calls.write(json.dumps(record) + "\n")
    if any((tool, word) in STDIN_READERS for word in argv):
        with (state / "stdin.jsonl").open("a") as kept:
            kept.write(json.dumps({"tool": tool, "stdin": sys.stdin.read()}) + "\n")
    scenario_file = state / "scenario.json"
    declared = json.loads(scenario_file.read_text()) if scenario_file.exists() else []
    best, best_score = None, None
    for entry in DEFAULTS + declared:
        prefix = entry["argv_prefix"]
        words = entry.get("contains", [])
        if (entry["tool"] == tool and argv[: len(prefix)] == prefix
                and all(word in argv for word in words)):
            score = (len(prefix), len(words))
            if best_score is None or score >= best_score:
                best, best_score = entry, score
    if through_a_proxy(tool, argv):
        sys.stderr.write("curl: (56) CONNECT tunnel failed, response 403\n")
        return 56
    if best is None:
        return 0
    if best.get("block") == "signal":
        wait_for_a_signal(state, tool)
    elif best.get("block"):
        # Simulates a long call: tells the test it started, then waits to be killed.
        (state / "blocked").write_text(tool)
        time.sleep(60)
    if best["exit"] == 0:
        for installed in best.get("installs", []):
            link = Path(os.environ["VAULTCENTER_SANDBOX_BIN"]) / installed
            double = Path(__file__).resolve().parent / installed
            if not link.exists():
                # python3 has no double: the package brings the real interpreter.
                link.symlink_to(double if double.exists() else sys.executable)
        if tool == "docker" and argv[:1] == ["cp"] and simulate_docker_cp(argv):
            return 1
        if tool == "curl" and "-o" in argv and argv[argv.index("-o") + 1] != "/dev/null":
            Path(argv[argv.index("-o") + 1]).write_text("simulated\n")
        if tool == "openssl" and argv[:1] == ["req"]:
            simulate_openssl_req(argv)
    if best.get("stderr"):
        sys.stderr.write(best["stderr"])
    sys.stdout.write(dynamic_output(tool, argv, best["stdout"]))
    return best["exit"]


if __name__ == "__main__":
    sys.exit(main())
