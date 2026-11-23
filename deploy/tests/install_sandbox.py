"""Sandbox for install.sh tests: a fake root, doubles for the system tools."""
import hashlib
import importlib.util
import io
import json
import os
import pty
import select
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from fcntl import ioctl
from pathlib import Path
from termios import TIOCSCTTY

HERE = Path(__file__).resolve().parent
DOUBLES = HERE / "install_sandbox"
INSTALL_DIR = HERE.parent / "install"
INSTALL_SH = INSTALL_DIR / "install.sh"
TOOLS = ("docker", "systemctl", "apt-get", "dkms", "modprobe", "curl", "uname",
         "ss", "hostname", "mokutil", "openssl", "id", "systemd-detect-virt", "pgrep",
         "apt-cache", "nproc", "df", "sysctl", "udevadm", "ip")
# Real, harmless tools reachable from a sandbox run. This is the whole PATH apart
# from the doubles: extend it deliberately, never with a tool that acts on the
# machine (docker, apt-get, systemctl, ...), those must be doubles.
ALLOWED_REAL_TOOLS = (
    "bash", "env", "python3", "sh", "cat", "chmod", "cp", "date", "dirname",
    "basename", "readlink", "grep", "sed", "awk", "tar", "sha256sum", "mktemp",
    "mkdir", "mv", "rm", "ln", "ls", "tr", "sort", "head", "tail", "tee",
    "touch", "wc", "cut", "stat", "sleep", "true", "false", "test", "gzip",
)
LOG = "var/log/vaultcenter-install.log"


class Sandbox:
    """A throw-away machine for install.sh.

    Every path the script uses lives under `.root` (VAULTCENTER_INSTALL_ROOT).
    PATH is the sandbox bin only: doubles plus ALLOWED_REAL_TOOLS.
    The system tools are doubles. `docker` is absent, as on a fresh VM,
    until `scenario('docker', ...)` is called.
    """

    def __init__(self, os_release: str = "12"):
        """`os_release` is the Debian VERSION_ID; any value may be given to
        simulate an unsupported release. The machine is healthy by default:
        systemd, cgroup v2, 6.1.0-test-amd64 kernel headers, 8 GiB of RAM."""
        self._tmp = tempfile.TemporaryDirectory(prefix="vc-install-")
        base = Path(self._tmp.name)
        self.root = base / "root"
        self._state = base / "state"
        self._bin = base / "bin"
        for directory in (self.root / "etc", self._state, self._bin,
                          self.root / "run/systemd/system",
                          self.root / "lib/modules/6.1.0-test-amd64/build",
                          self.root / "proc", self.root / "sys/fs/cgroup"):
            directory.mkdir(parents=True)
        (self.root / "sys/fs/cgroup/cgroup.controllers").write_text("cpu memory io\n")
        (self.root / "proc/meminfo").write_text("MemTotal:        8000000 kB\n")
        codename = {"11": "bullseye", "12": "bookworm", "13": "trixie"}.get(os_release, "unknown")
        (self.root / "etc" / "os-release").write_text(
            f'ID=debian\nVERSION_ID="{os_release}"\nVERSION_CODENAME={codename}\n')
        for tool in ALLOWED_REAL_TOOLS:
            real = shutil.which(tool)
            if real and tool not in TOOLS:
                (self._bin / tool).symlink_to(real)
        for tool in TOOLS:
            if tool != "docker":
                self._link(tool)
        self._scenario: list[dict] = []
        self._make_payload()

    def _make_payload(self) -> None:
        """What `docker cp` finds in the image: the host files, with the real
        dkms.conf and prepare-dkms-source.sh, and a small mhvtl source archive."""
        host = self._state / "payload" / "host"
        host.mkdir(parents=True)
        for name in ("dkms.conf", "prepare-dkms-source.sh"):
            shutil.copy2(INSTALL_DIR / name, host / name)
        deploy = HERE.parent
        for name in ("install.py", "storage_controller.py", "storage_paths.py", "vtl_state.py",
                     "vtl_storage.py", "storage_mounts.py", "vaultcenter-storage-host.service",
                     "vaultcenter-storage-mounts.service", "vaultcenter-mhvtl.service"):
            shutil.copy2(deploy / "host" / name, host / name)
        for name in ("compose.yaml", "auth.env.example"):
            shutil.copy2(deploy / "docker" / name, host / name)
        shutil.copy2(deploy.parent / "docs" / "install.md", host / "install.md")
        for name, text in (("vaultcenter-mhvtl.conf", "options mhvtl opts=0\n"),
                           ("99-vaultcenter-console.conf", "kernel.printk = 3 3 3 3\n"),
                           ("99-vaultcenter.rules", "# udev rules\n")):
            (host / name).write_text(text)
        # Stand-in for the updater shipped in the image: the real one would need a
        # whole fake appliance. It is a double like the others (see updater()).
        (host / "update.py").write_text(
            "import runpy, sys\n"
            f"sys.argv = ['_double.py', 'update.py', *sys.argv[1:]]\n"
            f"runpy.run_path({str(DOUBLES / '_double.py')!r}, run_name='__main__')\n")
        archive = self._state / "payload" / "source.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for name, text in (("Makefile", "all:\n"), ("kernel/Makefile", "all:\n")):
                data = text.encode()
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mtime = 0
                tar.addfile(info, io.BytesIO(data))
        self.mhvtl_archive_sha256 = hashlib.sha256(archive.read_bytes()).hexdigest()

    def _link(self, tool: str) -> None:
        link = self._bin / tool
        if not link.exists():
            link.symlink_to(DOUBLES / tool)

    def close(self) -> None:
        self._tmp.cleanup()

    def scenario(self, tool: str, argv_prefix: list[str], exit: int = 0,
                 stdout: str = "", contains: list[str] | None = None,
                 installs: list[str] | None = None, block: bool | str = False,
                 stderr: str = "") -> None:
        """Declare an answer. `contains`: words that must all be in argv, anywhere
        (the longest prefix, then the most words, then the last declared wins).
        `installs`: tools that appear on PATH once the call succeeds. `block`:
        True, the call never returns (until killed); "signal", it waits for
        SIGINT, SIGTERM or SIGHUP and then answers as declared."""
        self._scenario.append({"tool": tool, "argv_prefix": list(argv_prefix),
                               "exit": exit, "stdout": stdout,
                               "contains": list(contains or []),
                               "installs": list(installs or []), "block": block,
                               "stderr": stderr})
        (self._state / "scenario.json").write_text(json.dumps(self._scenario))
        if tool != "update.py":
            self._link(tool)

    def updater(self, result: dict | str | None, exit: int = 0, stderr: str = "",
                contains: list[str] | None = None, block: bool | str = False) -> None:
        """What the stand-in updater of the payload (host/update.py) answers: a JSON
        object (merged over a successful update's) or raw text, then `exit`.
        `contains` restricts the answer to calls with those words in argv."""
        if isinstance(result, dict):
            spec = importlib.util.spec_from_file_location("sandbox_double", DOUBLES / "_double.py")
            double = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(double)
            stdout = json.dumps({**double.UPDATED_RESULT, **result}) + "\n"
        else:
            stdout = result or ""
        self.scenario("update.py", [], exit=exit, stdout=stdout, contains=contains, stderr=stderr, block=block)

    def real_updater(self) -> None:
        """Ship the real deploy/host/update.py in the payload instead of its
        stand-in: install.sh then runs it against this sandbox, whose doubles
        answer its commands."""
        shutil.copy2(HERE.parent / "host" / "update.py", self._state / "payload" / "host" / "update.py")

    def remove_tool(self, tool: str) -> None:
        """Take a tool off PATH, as on a minimal machine that lacks it (declare an
        apt-get scenario with `installs` to bring it back)."""
        (self._bin / tool).unlink()

    def payload_file(self, name: str, content: str | bytes) -> Path:
        """Add a file to the host files of the image's payload (host/<name>)."""
        path = self._state / "payload" / "host" / name
        path.write_bytes(content if isinstance(content, bytes) else content.encode())
        return path

    def reset_scenario(self) -> None:
        """Forget every declared answer: only the healthy defaults remain (tools
        already put on PATH stay there)."""
        self._scenario = []
        (self._state / "scenario.json").write_text("[]")

    def calls(self, tool: str | None = None) -> list[list[str]]:
        """argv of every call to `tool`; with no tool, [tool, *argv] of all calls."""
        path = self._state / "calls.jsonl"
        if not path.exists():
            return []
        records = [json.loads(line) for line in path.read_text().splitlines()]
        if tool is None:
            return [[r["tool"], *r["argv"]] for r in records]
        return [r["argv"] for r in records if r["tool"] == tool]

    def start(self, *args: str, env: dict | None = None) -> subprocess.Popen:
        """Start install.sh without waiting (its own session: killpg reaches it and
        its children). Use wait_blocked() then signal it."""
        return subprocess.Popen(
            ["bash", str(INSTALL_SH), *args], env=self._env(env), text=True,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True)

    def wait_blocked(self, timeout: float = 20) -> bool:
        """True once a call declared with `block` has started."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if (self._state / "blocked").exists():
                return True
            time.sleep(0.05)
        return False

    def reset_calls(self) -> None:
        """Forget the calls recorded so far (to inspect a second run on its own)."""
        for name in ("calls.jsonl", "stdin.jsonl"):
            (self._state / name).unlink(missing_ok=True)

    def stdin(self, tool: str) -> list[str]:
        """Standard input of each call to `tool` that reads it (curl --data @-,
        docker login --password-stdin). Kept apart from calls.jsonl on purpose."""
        path = self._state / "stdin.jsonl"
        if not path.exists():
            return []
        records = [json.loads(line) for line in path.read_text().splitlines()]
        return [r["stdin"] for r in records if r["tool"] == tool]

    def call_envs(self, tool: str) -> list[dict]:
        """Recorded environment (proxy, DEBIAN_FRONTEND, DOCKER_CONFIG) of each call."""
        path = self._state / "calls.jsonl"
        if not path.exists():
            return []
        records = [json.loads(line) for line in path.read_text().splitlines()]
        return [r.get("env", {}) for r in records if r["tool"] == tool]

    def provide_docker(self) -> None:
        """Docker already installed, with its compose plugin."""
        self.scenario("docker", ["compose", "version"])

    def log(self) -> str:
        path = self.root / LOG
        return path.read_text() if path.exists() else ""

    def _env(self, env: dict | None) -> dict:
        base = {
            "PATH": str(self._bin),
            "HOME": str(self.root),
            "LANG": "C.UTF-8",
            "VAULTCENTER_INSTALL_ROOT": str(self.root),
            "VAULTCENTER_SANDBOX_DIR": str(self._state),
            "VAULTCENTER_SANDBOX_BIN": str(self._bin),
            "VAULTCENTER_SANDBOX_PYTHON": sys.executable,
            # The health wait polls three times without sleeping.
            "VAULTCENTER_INSTALL_HEALTH_ATTEMPTS": "3",
            "VAULTCENTER_INSTALL_HEALTH_INTERVAL": "0",
        }
        base.update(env or {})
        return base

    def run(self, *args: str, tty: bool = False, env: dict | None = None,
            tty_input: str = "") -> subprocess.CompletedProcess:
        """Run install.sh.

        Without tty the script has no controlling terminal and no terminal on
        stdout. With tty it is fed on stdin, as `curl | bash` does, and gets a
        pseudo-terminal as controlling terminal (/dev/tty) and as stdout/stderr;
        `tty_input` is typed on it and its output comes back in `.stdout`.
        """
        environment = self._env(env)
        if not tty:
            return subprocess.run(
                ["bash", str(INSTALL_SH), *args], env=environment, text=True,
                stdin=subprocess.DEVNULL, capture_output=True,
                start_new_session=True, timeout=30, check=False)
        master, slave = pty.openpty()

        def become_controlling_terminal() -> None:
            ioctl(slave, TIOCSCTTY, 0)

        try:
            with INSTALL_SH.open("rb") as script:
                child = subprocess.Popen(
                    ["bash", "-s", "--", *args], env=environment, stdin=script,
                    stdout=slave, stderr=slave, start_new_session=True,
                    preexec_fn=become_controlling_terminal)
            os.write(master, tty_input.encode())
            output = self._drain(master, child)
        finally:
            os.close(slave)
            os.close(master)
        return subprocess.CompletedProcess(
            child.args, child.returncode, output.decode(errors="replace"), "")

    @staticmethod
    def _drain(master: int, child: subprocess.Popen) -> bytes:
        output = b""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                try:
                    chunk = os.read(master, 4096)
                except OSError:
                    break
                output += chunk
            elif child.poll() is not None:
                break
        child.wait(timeout=5)
        return output
