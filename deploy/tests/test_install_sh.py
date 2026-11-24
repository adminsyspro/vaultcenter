"""install.sh: options, rendering, log and mode detection (steps are not run here)."""
import hashlib
import json
import os
import re
import signal
import sqlite3
import stat
import shutil
import unittest
from contextlib import closing
from pathlib import Path

from install_sandbox import Sandbox

TOKEN = "vc_tok_SECRETVALUE123"
REGISTRY_TOKEN = "ghp_REGISTRYSECRET456"
ESC = "\x1b"
# The installation guide, where the image ships it on the host.
LOCAL_DOC = "/usr/local/share/vaultcenter/0.1.0/host/install.md"


def fail_host_service(sandbox):
    """Stop a run at the host service step (the ninth): for tests about what
    precedes it, whose records a complete run would erase."""
    sandbox.scenario("docker", ["compose", "-f"], contains=["config", "json"], exit=1)


class InstallShTest(unittest.TestCase):
    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)

    def changes(self):
        """Calls that are not read-only probes of the machine."""
        return [c for c in self.sb.calls() if c[0] != "id"]

    def mode(self):
        found = re.search(r" mode=(\w+)$", self.sb.log(), re.M)
        return found.group(1) if found else None

    def make(self, *paths):
        for path in paths:
            target = self.sb.root / path
            if path.endswith(("compose.json", "state")):
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("")
            else:
                target.mkdir(parents=True, exist_ok=True)

    def test_sandbox_reaches_no_real_system_tool(self):
        path = self.sb._env(None)["PATH"]
        self.assertNotIn(":", path)
        self.assertIsNone(shutil.which("docker", path=path))
        self.assertIsNone(shutil.which("whoami", path=path))  # real, not allowed
        self.sb.scenario("docker", ["ps"])
        self.assertIsNotNone(shutil.which("docker", path=path))
        for tool in ("apt-get", "systemctl", "id", "uname"):
            real = os.path.realpath(shutil.which(tool, path=path))
            self.assertTrue(real.endswith("/install_sandbox/" + tool), real)

    def test_registry_user_alone_stops_before_any_change(self):
        result = self.sb.run("--registry-user", "lab", "--yes")
        self.assertEqual(result.returncode, 2)
        self.assertIn("--registry-token", result.stderr)
        self.assertEqual(self.changes(), [])

    def test_token_and_registry_token_exclude_each_other(self):
        result = self.sb.run("--token", TOKEN, "--registry-token", REGISTRY_TOKEN,
                             "--registry-user", "u", "--yes")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(self.changes(), [])

    def test_registry_token_requires_registry_user(self):
        result = self.sb.run("--registry-token", REGISTRY_TOKEN, "--yes")
        self.assertEqual(result.returncode, 2)
        self.assertIn("--registry-user", result.stderr)

    def test_unknown_option_is_refused(self):
        result = self.sb.run("--token", TOKEN, "--frobnicate")
        self.assertEqual(result.returncode, 2)
        self.assertIn("--frobnicate", result.stderr)

    def test_bad_version_is_refused(self):
        for version in ("latest", "1.2", "1.2.0-", "1.2.0-rc..1", "1.2.0+build", "v1.2.0", "1.2.0-rc_1"):
            with self.subTest(version):
                result = self.sb.run("--token", TOKEN, "--version", version, "--yes")
                self.assertEqual(result.returncode, 2)
                self.assertIn("X.Y.Z-<pre-release>", result.stderr)
        self.assertEqual(self.changes(), [])

    def test_pre_release_version_is_installed(self):
        version = "0.2.0-rc.1"
        self.sb.scenario("docker", ["image", "inspect"],
                         contains=['{{index .Config.Labels "org.opencontainers.image.version"}}'],
                         stdout=version + "\n")
        self.sb.scenario("curl", [], contains=["https://127.0.0.1:8443/health"],
                         stdout=json.dumps({"status": "ok", "version": version}))
        result = self.sb.run("--token", TOKEN, "--version", version, "--yes")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(["pull", f"ghcr.io/adminsyspro/vaultcenter:{version}"], self.sb.calls("docker"))
        self.assertTrue((self.sb.root / "usr/local/share/vaultcenter" / version / "host/install.py").is_file())
        self.assertIn(f"appliance healthy, version {version}", result.stdout)

    def test_unknown_option_never_echoes_a_value(self):
        for option in ("--token=" + TOKEN, "--registry-token=" + REGISTRY_TOKEN):
            with self.subTest(option.split("=")[0]):
                result = self.sb.run(option, "--yes")
                self.assertEqual(result.returncode, 2)
                self.assertIn("unknown option: " + option.split("=")[0], result.stderr)
                self.assertNotIn(option.split("=")[1], result.stdout + result.stderr)

    def test_question_is_worded_for_the_operator(self):
        cases = {"install": ([], (), "Install VaultCenter on this machine?"),
                 "resume": (["var/lib/vaultcenter-install/state"], (),
                            "Resume the interrupted installation of VaultCenter on this machine?"),
                 "update": (["etc/vaultcenter-host/compose.json"], (),
                            "Update the VaultCenter appliance on this machine?"),
                 "rollback": (["etc/vaultcenter-host/compose.json"], ("--resume-rollback",),
                              "Cancel the interrupted update of this VaultCenter appliance (rollback)?")}
        for name, (paths, options, question) in cases.items():
            with self.subTest(name):
                sb = Sandbox()
                self.addCleanup(sb.close)
                self.sb = sb
                self.make(*paths)
                result = sb.run("--token", TOKEN, *options, tty=True, tty_input="n\n")
                self.assertEqual(result.returncode, 3, result.stdout)
                self.assertIn(question + " [y/N]", result.stdout)
                self.assertNotRegex(result.stdout, r"Mode (install|resume|update)\b")

    def test_not_root_is_refused(self):
        self.sb.scenario("id", ["-u"], stdout="1000\n")
        result = self.sb.run("--token", TOKEN, "--yes")
        self.assertEqual(result.returncode, 3)
        self.assertIn("root", result.stderr)

    def test_modes(self):
        cases = [
            ("install", []),
            ("update", ["etc/vaultcenter-host/compose.json"]),
            ("legacy", ["etc/freezer-host"]),
            ("resume", ["var/lib/vaultcenter-install/state"]),
            ("mixed", ["etc/vaultcenter-host"]),
        ]
        for expected, paths in cases:
            with self.subTest(expected):
                sb = Sandbox()
                self.addCleanup(sb.close)
                self.sb = sb
                self.make(*paths)
                sb.run("--token", TOKEN, "--yes")
                self.assertEqual(self.mode(), expected)

    def test_container_presence_is_asked_to_docker(self):
        self.sb.scenario("docker", ["ps"], stdout="freezer-appliance-1\n")
        result = self.sb.run("--token", TOKEN, "--yes")
        self.assertEqual(self.mode(), "legacy")
        self.assertEqual(result.returncode, 3)

    def test_vaultcenter_container_alone_is_mixed(self):
        self.sb.scenario("docker", ["ps"], stdout="vaultcenter-appliance-1\n")
        result = self.sb.run("--token", TOKEN, "--yes")
        self.assertEqual(self.mode(), "mixed")
        self.assertEqual(result.returncode, 3)
        self.assertIn("vaultcenter-appliance-1", result.stderr)

    def test_legacy_installation_changes_nothing(self):
        self.make("etc/freezer-host")
        result = self.sb.run("--token", TOKEN, "--yes")
        self.assertEqual(result.returncode, 3)
        self.assertIn("former product name", result.stderr)
        self.assertIn("does not convert it", result.stderr)
        self.assertIn("/etc/freezer-host", result.stderr)
        self.assertNotIn("migration-to-vaultcenter", result.stderr)
        self.assertEqual(self.changes(), [])

    def test_without_tty_and_without_yes_is_refused(self):
        result = self.sb.run("--token", TOKEN)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--yes", result.stderr)
        self.assertEqual(self.changes(), [])

    def test_yes_runs_the_whole_installation(self):
        result = self.sb.run("--token", TOKEN, "--yes")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("not implemented yet", result.stdout + result.stderr)

    def test_question_is_read_from_tty_not_stdin(self):
        # The script itself is on stdin (curl | bash): reading stdin would eat it.
        result = self.sb.run("--token", TOKEN, tty=True, tty_input="y\n")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.mode(), "install")

    def test_answering_no_on_tty_cancels(self):
        result = self.sb.run("--token", TOKEN, tty=True, tty_input="n\n")
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertEqual(self.changes(), [])

    def test_quiet_prints_one_line_per_step(self):
        result = self.sb.run("--token", TOKEN, "--yes", "--quiet")
        # The final box follows the eleven step lines, quiet or not.
        lines = [line for line in result.stdout.splitlines() if re.match(r"^\[\d+/\d+\] ", line)]
        self.assertEqual(len(lines), 11)
        for line in lines:
            self.assertRegex(line, r"^\[\d+/\d+\] .+: .+$")
        self.assertTrue(lines[0].startswith("[1/11] "))
        self.assertTrue(result.stdout.splitlines()[0].startswith("[1/11] "))
        self.assertIn("https://", result.stdout)

    def test_terminal_is_decorated_and_no_color_is_respected(self):
        decorated = self.sb.run("--token", TOKEN, "--yes", tty=True)
        self.assertIn(ESC, decorated.stdout)
        plain = self.sb.run("--token", TOKEN, "--yes", tty=True, env={"NO_COLOR": "1"})
        self.assertNotIn(ESC, plain.stdout)
        self.assertIn("Docker", plain.stdout)

    def test_no_color_is_respected_without_terminal_too(self):
        result = self.sb.run("--token", TOKEN, "--yes", env={"NO_COLOR": "1"})
        self.assertNotIn(ESC, result.stdout + result.stderr)

    def test_log_is_private(self):
        self.sb.run("--token", TOKEN, "--yes")
        log = self.sb.root / "var/log/vaultcenter-install.log"
        self.assertEqual(stat.S_IMODE(os.stat(log).st_mode), 0o600)

    def test_secrets_never_reach_the_log(self):
        self.sb.run("--registry-token", REGISTRY_TOKEN, "--registry-user", "lab", "--yes")
        self.sb.run("--token", TOKEN, "--yes")
        log = self.sb.log()
        self.assertIn("mode=install", log)
        self.assertNotIn(TOKEN, log)
        self.assertNotIn(REGISTRY_TOKEN, log)


REFUSALS = {"debian_11": "Debian 12 or 13", "arm64": "amd64", "cgroup_v1": "cgroup v2",
            "lxc": "container", "port_8443": "8443", "port_3260": "3260",
            "host_tgt": "tgt", "secure_boot": "Secure Boot",
            "headers_missing": "reboot"}
PROBES = {"id", "uname", "ss", "pgrep", "mokutil", "apt-cache", "systemd-detect-virt",
          "nproc", "df", "curl"}


def refreshes_apt_lists(call):
    """`apt-get ... update`: the one command of the preflight that is not a pure
    read (apt-cache answers from the lists)."""
    return call[0] == "apt-get" and "update" in call and "install" not in call


class PreflightTest(unittest.TestCase):
    ARGS = ("--token", TOKEN, "--yes")

    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)

    def break_fact(self, name, sb=None):
        sb = sb or self.sb
        if name == "debian_11":
            (sb.root / "etc/os-release").write_text('ID=debian\nVERSION_ID="11"\n')
        elif name == "arm64":
            sb.scenario("uname", ["-m"], stdout="aarch64\n")
        elif name == "cgroup_v1":
            (sb.root / "sys/fs/cgroup/cgroup.controllers").unlink()
        elif name == "no_systemd":
            (sb.root / "run/systemd/system").rmdir()
        elif name == "lxc":
            sb.scenario("systemd-detect-virt", ["--container"], stdout="lxc\n")
        elif name == "rootless":
            sb.scenario("docker", ["ps"])
            sb.scenario("docker", ["info"], stdout="[name=seccomp,profile=builtin name=rootless]\n")
        elif name == "port_8443":
            sb.scenario("ss", [], stdout="LISTEN 0 4096 0.0.0.0:8443 0.0.0.0:*\n")
        elif name == "port_3260":
            sb.scenario("ss", [], stdout="LISTEN 0 4096 [::]:3260 [::]:*\n")
        elif name == "host_tgt":
            sb.scenario("systemctl", ["is-active", "tgt.service"], stdout="active\n")
        elif name == "host_vtltape":
            sb.scenario("pgrep", ["-x", "vtltape"], stdout="1234\n")
        elif name == "secure_boot":
            sb.scenario("mokutil", ["--sb-state"], stdout="SecureBoot enabled\n")
        elif name == "headers_missing":
            (sb.root / "lib/modules/6.1.0-test-amd64/build").rmdir()
            sb.scenario("apt-cache", ["policy"],
                        stdout="linux-headers-6.1.0-test-amd64:\n  Candidate: (none)\n")
        elif name == "cpu_1":
            sb.scenario("nproc", [], stdout="1\n")
        elif name == "ram_2g":
            (sb.root / "proc/meminfo").write_text("MemTotal:        2097152 kB\n")
        elif name == "disk_10g":
            sb.scenario("df", ["-Pk"], stdout="Filesystem 1024-blocks Used Available Capacity Mounted on\n"
                                              "/dev/sda1 20971520 10485760 10485760 50% /\n")
        elif name == "offline":
            sb.scenario("curl", [], exit=6, stdout="000")
        else:
            raise AssertionError(name)

    def modifying(self):
        return [c for c in self.sb.calls()
                if c[0] not in PROBES and not refreshes_apt_lists(c)
                and not (c[0] == "docker" and c[1] in ("ps", "info"))
                and not (c[0] == "systemctl" and c[1] == "is-active")]

    def tree(self):
        return {str(p.relative_to(self.sb.root)) for p in self.sb.root.rglob("*")}

    def assert_refused(self, result, *needles):
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        for needle in needles:
            self.assertIn(needle, result.stderr)
        self.assertEqual(self.modifying(), [])

    def test_healthy_machine_passes_and_records_the_step(self):
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Preflight checks: ", result.stdout)
        # A finished installation keeps no step record: the next run is an update.
        state = (self.sb.root / "var/lib/vaultcenter-install/state").read_text()
        self.assertNotIn("done=", state)
        # The preflight only reads, apart from refreshing the apt lists: the
        # first call of the next step is `hostname`.
        calls = self.sb.calls()
        first_step_two = [c[0] for c in calls].index("hostname")
        self.assertEqual(len([c for c in calls[:first_step_two] if refreshes_apt_lists(c)]), 1)
        self.assertEqual([c for c in calls[:first_step_two]
                          if c[0] not in PROBES and not refreshes_apt_lists(c)
                          and not (c[0] == "docker" and c[1] in ("ps", "info"))
                          and not (c[0] == "systemctl" and c[1] == "is-active")], [])

    def test_debian_13_passes(self):
        sb = Sandbox("13")
        self.addCleanup(sb.close)
        self.assertNotEqual(sb.run(*self.ARGS).returncode, 3)

    def test_safety_refusals(self):
        for name, needle in REFUSALS.items():
            with self.subTest(name):
                self.sb = Sandbox()
                self.addCleanup(self.sb.close)
                self.break_fact(name)
                before = self.tree()
                result = self.sb.run(*self.ARGS)
                self.assert_refused(result, needle)
                created = self.tree() - before
                self.assertTrue(all(p in ("var", "var/lib") or p.startswith("var/log") or p.startswith("var/lib/vaultcenter-install")
                                    for p in created), created)
                self.assertNotIn("done=preflight",
                                 (self.sb.root / "var/lib/vaultcenter-install/state").read_text()
                                 if (self.sb.root / "var/lib/vaultcenter-install/state").exists() else "")

    def test_other_safety_refusals(self):
        for name, needle in {"no_systemd": "systemd", "rootless": "rootless",
                             "host_vtltape": "mhvtl"}.items():
            with self.subTest(name):
                self.sb = Sandbox()
                self.addCleanup(self.sb.close)
                self.break_fact(name)
                self.assert_refused(self.sb.run(*self.ARGS), needle)

    def test_all_refusals_are_listed_at_once(self):
        self.break_fact("debian_11")
        self.break_fact("port_8443")
        self.break_fact("cpu_1")
        result = self.sb.run(*self.ARGS)
        self.assert_refused(result, "Debian 12 or 13", "8443", "CPU")
        lines = [l for l in result.stderr.splitlines() if l.lstrip().startswith("- ")]
        self.assertEqual(len(lines), 3, result.stderr)
        self.assertIn("8443", self.sb.log())

    def test_skip_preflight_skips_sizing_only(self):
        self.break_fact("cpu_1")
        self.break_fact("ram_2g")
        self.break_fact("offline")
        result = self.sb.run(*self.ARGS, "--skip-preflight")
        # Past the preflight; the offline machine then cannot fetch the Docker key.
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("Docker", result.stderr)
        self.assertEqual(self.sb.calls("nproc"), [])
        offline_probes = ("https://ghcr.io/v2/", "https://deb.debian.org/",
                          "https://download.docker.com/")
        probes = [c for c in self.sb.calls("curl") if any(url in c for url in offline_probes)]
        self.assertEqual(probes, [])
        self.break_fact("port_3260")
        self.sb.reset_calls()
        self.assert_refused(self.sb.run(*self.ARGS, "--skip-preflight"), "3260")

    def test_sizing_refusals(self):
        for name, needle in {"cpu_1": "CPU", "ram_2g": "RAM", "disk_10g": "20 GiB"}.items():
            with self.subTest(name):
                self.sb = Sandbox()
                self.addCleanup(self.sb.close)
                self.break_fact(name)
                self.assert_refused(self.sb.run(*self.ARGS), needle)
        with self.subTest("offline"):
            self.sb = Sandbox()
            self.addCleanup(self.sb.close)
            self.break_fact("offline")
            self.assert_refused(self.sb.run(*self.ARGS), "ghcr.io", "deb.debian.org",
                                "download.docker.com")

    def test_sizing_tolerances(self):
        (self.sb.root / "proc/meminfo").write_text("MemTotal:        3900000 kB\n")  # ~3.7 GiB
        self.sb.scenario("curl", [], stdout="401")  # the registry answers 401
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 0)

    def test_headers_available_from_the_archive_are_accepted(self):
        (self.sb.root / "lib/modules/6.1.0-test-amd64/build").rmdir()
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 0)

    def test_apt_lists_are_refreshed_before_apt_cache_is_asked(self):
        # A cloud image has empty lists: apt-cache would know no package at all.
        (self.sb.root / "lib/modules/6.1.0-test-amd64/build").rmdir()
        self.sb.provide_docker()
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 0)
        names = [c[0] for c in self.sb.calls()]
        update = next(i for i, c in enumerate(self.sb.calls()) if refreshes_apt_lists(c))
        self.assertLess(update, names.index("apt-cache"))
        self.assertIn("DPkg::Lock::Timeout=300", self.sb.calls()[update])
        # Once per run when no repository is added: the kernel module step does
        # not refresh them again.
        self.assertEqual(len([c for c in self.sb.calls() if refreshes_apt_lists(c)]), 1)

    def test_missing_headers_refusal_says_what_to_do(self):
        self.break_fact("headers_missing")
        result = self.sb.run(*self.ARGS)
        self.assert_refused(result, "linux-headers-6.1.0-test-amd64", "apt-get update",
                            "update the kernel", "reboot")
        self.assertNotIn("sans doute", result.stderr)

    def test_failed_refresh_of_the_apt_lists_is_a_cause_of_refusal(self):
        self.sb.scenario("apt-get", [], contains=["update"], exit=100,
                         stdout="E: Failed to fetch http://deb.debian.org/debian/dists/bookworm/InRelease\n")
        result = self.sb.run(*self.ARGS)
        self.assert_refused(result, "apt-get update failed", "Failed to fetch")
        self.assertEqual(len([c for c in self.sb.calls() if refreshes_apt_lists(c)]), 1)

    def test_package_that_follows_the_kernel_must_be_installable(self):
        self.sb.scenario("apt-cache", ["policy", "linux-headers-amd64"],
                         stdout="linux-headers-amd64:\n  Installed: (none)\n  Candidate: (none)\n")
        self.assert_refused(self.sb.run(*self.ARGS), "linux-headers-amd64", "kernel update")
        self.assertIn(["policy", "linux-headers-amd64"], self.sb.calls("apt-cache"))

    def test_update_mode_with_installed_headers_asks_apt_nothing(self):
        compose = self.sb.root / "etc/vaultcenter-host/compose.json"
        compose.parent.mkdir(parents=True)
        compose.write_text("")
        self.sb.provide_docker()
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 0)
        self.assertEqual(self.sb.calls("apt-get") + self.sb.calls("apt-cache"), [])

    def update_mode(self):
        compose = self.sb.root / "etc/vaultcenter-host/compose.json"
        compose.parent.mkdir(parents=True)
        compose.write_text("")
        self.sb.provide_docker()

    def test_update_is_not_measured_against_the_sizing_of_a_fresh_installation(self):
        # An appliance in service: its disk holds its data, its sizing is what it is.
        self.update_mode()
        for fact in ("cpu_1", "ram_2g", "disk_10g"):
            self.break_fact(fact)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sb.calls("nproc") + self.sb.calls("df"), [])
        probes = [c[-1] for c in self.sb.calls("curl") if c[-1].startswith("https://")]
        for url in ("https://ghcr.io/v2/", "https://deb.debian.org/", "https://download.docker.com/"):
            self.assertIn(url, probes)

    def test_update_still_needs_the_network(self):
        self.update_mode()
        self.break_fact("offline")
        self.assert_refused(self.sb.run(*self.ARGS), "ghcr.io")

    def test_resume_is_still_measured_like_an_installation(self):
        self.start_resume()
        self.break_fact("disk_10g")
        self.assert_refused(self.sb.run(*self.ARGS), "20 GiB")

    def missing(self, tool, *packages):
        """`tool` is not on the machine; apt brings it with its package."""
        self.sb.remove_tool(tool)
        self.sb.scenario("apt-get", [], contains=["install", *packages], installs=[tool])

    def tool_installs(self):
        return [c[c.index("-y") + 1:] for c in self.sb.calls("apt-get") if "install" in c and "dkms" not in c
                and "docker-ce" not in c]

    def test_missing_tools_are_installed_before_their_first_use(self):
        cases = {"openssl": ["openssl"], "python3": ["python3"], "curl": ["curl", "ca-certificates"]}
        for tool, packages in cases.items():
            with self.subTest(tool):
                self.sb = Sandbox()
                self.addCleanup(self.sb.close)
                self.sb.provide_docker()
                self.missing(tool, *packages)
                result = self.sb.run(*self.ARGS)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.tool_installs(), [packages])
                # Installed within the preflight, before the step that follows it.
                names = [c[0] for c in self.sb.calls()]
                install = next(i for i, c in enumerate(self.sb.calls())
                               if c[0] == "apt-get" and "install" in c and packages[0] in c)
                self.assertLess(install, names.index("hostname"))
                if tool == "curl":
                    self.assertLess(install, names.index("curl"))

    def test_nothing_is_installed_on_a_machine_the_safety_checks_refuse(self):
        self.missing("openssl", "openssl")
        self.break_fact("port_8443")
        self.assert_refused(self.sb.run(*self.ARGS), "8443", "nothing has been changed")
        self.assertEqual(self.tool_installs(), [])

    def test_missing_tool_whose_package_is_unknown_is_a_cause_of_refusal(self):
        self.sb.remove_tool("openssl")
        self.sb.scenario("apt-cache", ["policy", "openssl"], stdout="openssl:\n  Candidate: (none)\n")
        self.assert_refused(self.sb.run(*self.ARGS), "openssl", "cannot be found")

    def test_sizing_refusal_after_tools_says_what_was_installed(self):
        self.missing("openssl", "openssl")
        self.break_fact("cpu_1")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn("only the packages openssl have been installed", result.stderr)
        self.assertIn("CPU", result.stderr)
        self.assertEqual(self.tool_installs(), [["openssl"]])

    def test_update_does_not_install_openssl(self):
        self.update_mode()
        self.sb.remove_tool("openssl")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sb.calls("apt-get"), [])

    def test_resume_runs_the_preflight_again(self):
        state = self.sb.root / "var/lib/vaultcenter-install/state"
        state.parent.mkdir(parents=True)
        state.write_text("done=preflight\n")
        self.break_fact("port_8443")
        self.assert_refused(self.sb.run(*self.ARGS), "8443")

    def busy_machine(self):
        self.sb.scenario("ss", [], stdout="LISTEN 0 4096 0.0.0.0:8443 0.0.0.0:*\n"
                                          "LISTEN 0 4096 0.0.0.0:3260 0.0.0.0:*\n")
        self.sb.scenario("systemctl", ["is-active", "tgt.service"], stdout="active\n")
        self.sb.scenario("pgrep", ["-x", "tgtd"], stdout="42\n")

    def start_resume(self):
        state = self.sb.root / "var/lib/vaultcenter-install/state"
        state.parent.mkdir(parents=True)
        state.write_text("done=preflight\n")

    def test_resume_with_own_appliance_tolerates_its_ports_and_daemons(self):
        self.start_resume()
        self.sb.scenario("docker", ["ps"], stdout="vaultcenter-appliance-1\n")
        self.busy_machine()
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("8443", result.stderr)

    def test_resume_with_own_appliance_still_checks_the_rest(self):
        self.start_resume()
        self.sb.scenario("docker", ["ps"], stdout="vaultcenter-appliance-1\n")
        self.busy_machine()
        self.break_fact("secure_boot")
        self.assert_refused(self.sb.run(*self.ARGS), "Secure Boot")

    def test_resume_without_the_appliance_refuses_a_busy_machine(self):
        self.start_resume()
        self.sb.scenario("docker", ["ps"], stdout="")
        self.busy_machine()
        self.assert_refused(self.sb.run(*self.ARGS), "8443", "3260", "tgt")

    def test_update_mode_tolerates_busy_ports_but_not_other_causes(self):
        compose = self.sb.root / "etc/vaultcenter-host/compose.json"
        compose.parent.mkdir(parents=True)
        compose.write_text("")
        self.busy_machine()
        # The preflight passes and the update goes through (the appliance
        # legitimately holds its own ports).
        self.sb.provide_docker()
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("8443", result.stderr)
        self.break_fact("secure_boot")
        self.sb.reset_calls()
        self.assert_refused(self.sb.run(*self.ARGS), "Secure Boot")

    def test_secure_boot_without_mokutil_state_is_accepted(self):
        self.sb.scenario("mokutil", ["--sb-state"], exit=1,
                         stdout="EFI variables are not supported on this system\n")
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 0)


REGISTRY_PASSWORD = "ghp_SITEPASSWORD789"
VALIDATE_URL = "https://vaultcenter.io/api/v1/install/validate"
DIGEST = "ghcr.io/adminsyspro/vaultcenter@sha256:" + "0123456789abcdef" * 4
IMAGE_STEPS = ("credentials", "docker", "image", "payload")


class ImageStepsTest(unittest.TestCase):
    ARGS = ("--token", TOKEN, "--yes")

    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)

    def state(self):
        path = self.sb.root / "var/lib/vaultcenter-install/state"
        return path.read_text().splitlines() if path.exists() else []

    def docker_apt_calls(self):
        """Package installs of the Docker steps: the kernel module step has its own
        install, and the single `apt-get update` of the run is counted apart."""
        return [c for c in self.sb.calls("apt-get") if "dkms" not in c and "update" not in c]

    def apt_installs(self):
        return [c for c in self.docker_apt_calls() if "install" in c]

    def site_calls(self):
        return [c for c in self.sb.calls("curl") if VALIDATE_URL in c]

    def test_healthy_machine_runs_the_four_steps(self):
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("Host storage service", result.stderr)
        state = self.state()
        for name in IMAGE_STEPS:
            self.assertEqual(state.count(f"done={name}"), 1, state)
        self.assertIn(f"image={DIGEST}", state)
        self.assertIn("version=0.1.0", state)

    def test_token_is_exchanged_with_the_site(self):
        self.sb.run(*self.ARGS)
        calls = self.site_calls()
        self.assertEqual(len(calls), 1)
        self.assertIn("-X", calls[0])
        self.assertIn("POST", calls[0])
        self.assertIn("Content-Type: application/json", calls[0])
        self.assertIn("@-", calls[0])
        body = json.loads(self.sb.stdin("curl")[0])
        self.assertEqual(body, {"token": TOKEN, "hostname": "vaultcenter-test", "os": "debian-12"})

    def test_without_token_the_public_image_is_pulled_without_credentials(self):
        fail_host_service(self.sb)
        self.sb.run("--yes")
        self.assertEqual([c for c in self.sb.calls("curl") if "vaultcenter.io" in " ".join(c)], [])
        docker = self.sb.calls("docker")
        self.assertEqual([c for c in docker if c[0] in ("login", "logout")], [])
        self.assertEqual(len([c for c in docker if c[0] == "pull"]), 1)
        state = self.state()
        for name in IMAGE_STEPS:
            self.assertEqual(state.count(f"done={name}"), 1, state)
        self.assertIn(f"image={DIGEST}", state)

    def test_refused_anonymous_pull_says_the_version_is_not_public(self):
        self.sb.scenario("docker", ["pull"], exit=1,
                         stdout="Error response from daemon: denied")
        result = self.sb.run("--yes")
        self.assertEqual(result.returncode, 4)
        self.assertIn("without credentials", result.stderr)
        self.assertEqual([c for c in self.sb.calls("docker") if c[0] == "login"], [])

    def test_os_field_follows_the_release(self):
        sb = Sandbox("13")
        self.addCleanup(sb.close)
        sb.run(*self.ARGS)
        self.assertEqual(json.loads(sb.stdin("curl")[0])["os"], "debian-13")

    def test_refused_token_stops_with_the_site_message(self):
        self.sb.scenario("curl", [], contains=[VALIDATE_URL],
                         stdout='{"valid": false, "message": "token expired"}')
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("token expired", result.stderr)
        self.assertNotIn("done=credentials", self.state())
        self.assertEqual([c for c in self.sb.calls("apt-get") if "install" in c], [])

    def test_unreachable_site_is_a_step_failure(self):
        self.sb.scenario("curl", [], contains=[VALIDATE_URL], exit=6, stdout="")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("vaultcenter.io", result.stderr)

    def test_unreadable_answer_is_a_step_failure(self):
        self.sb.scenario("curl", [], contains=[VALIDATE_URL], stdout="<html>502</html>")
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)

    def test_registry_token_skips_the_site(self):
        fail_host_service(self.sb)
        self.sb.run("--registry-token", REGISTRY_TOKEN, "--registry-user", "lab", "--yes")
        self.assertEqual(self.site_calls(), [])
        self.assertEqual([c for c in self.sb.calls("curl") if "vaultcenter.io" in " ".join(c)], [])
        self.assertEqual(self.sb.stdin("docker"), [REGISTRY_TOKEN])
        login = [c for c in self.sb.calls("docker") if c[0] == "login"][0]
        self.assertIn("lab", login)
        self.assertIn("ghcr.io", login)

    def test_login_uses_the_site_credentials_on_stdin(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        login = [c for c in self.sb.calls("docker") if c[0] == "login"]
        self.assertEqual(len(login), 1)
        self.assertIn("--password-stdin", login[0])
        self.assertIn("vc-customer", login[0])
        self.assertEqual(self.sb.stdin("docker"), [REGISTRY_PASSWORD])

    def test_password_is_never_an_argument(self):
        for args, secrets in ((self.ARGS, (TOKEN, REGISTRY_PASSWORD)),
                              (("--registry-token", REGISTRY_TOKEN, "--registry-user", "lab", "--yes"),
                               (REGISTRY_TOKEN,))):
            sb = Sandbox()
            self.addCleanup(sb.close)
            sb.run(*args)
            for call in sb.calls():
                for secret in secrets:
                    self.assertFalse(any(secret in word for word in call), call)
            recorded = (sb._state / "calls.jsonl").read_text()
            for secret in secrets:
                self.assertNotIn(secret, recorded)
                self.assertNotIn(secret, sb.log())

    def test_site_secrets_never_reach_the_log_nor_the_terminal(self):
        result = self.sb.run(*self.ARGS)
        for secret in (TOKEN, REGISTRY_PASSWORD):
            self.assertNotIn(secret, self.sb.log())
            self.assertNotIn(secret, result.stdout + result.stderr)

    def test_docker_is_installed_from_the_docker_repository_when_missing(self):
        self.sb.run(*self.ARGS)
        keyring = self.sb.root / "etc/apt/keyrings/docker.asc"
        self.assertTrue(keyring.is_file())
        self.assertTrue([c for c in self.sb.calls("curl")
                         if "https://download.docker.com/linux/debian/gpg" in c])
        source = (self.sb.root / "etc/apt/sources.list.d/docker.list").read_text()
        self.assertIn("https://download.docker.com/linux/debian bookworm stable", source)
        self.assertIn("signed-by=/etc/apt/keyrings/docker.asc", source)
        self.assertIn("arch=amd64", source)
        installs = self.apt_installs()
        self.assertEqual(len(installs), 1)
        for package in ("docker-ce", "docker-ce-cli", "containerd.io", "docker-compose-plugin"):
            self.assertIn(package, installs[0])
        self.assertIn(["enable", "--now", "docker"],
                      [c for c in self.sb.calls("systemctl") if c[0] == "enable"])
        self.assertIn(["update"], [[w for w in c if not w.startswith("-o") and w != "DPkg::Lock::Timeout=300"]
                                   for c in self.sb.calls("apt-get")])

    def test_debian_13_uses_trixie(self):
        sb = Sandbox("13")
        self.addCleanup(sb.close)
        sb.run(*self.ARGS)
        self.assertIn(" trixie stable", (sb.root / "etc/apt/sources.list.d/docker.list").read_text())

    def test_every_apt_call_waits_for_the_lock_and_is_not_interactive(self):
        self.sb.run(*self.ARGS)
        calls = self.sb.calls("apt-get")
        self.assertGreaterEqual(len(calls), 2)
        for call in calls:
            self.assertIn("DPkg::Lock::Timeout=300", call)
        for env in self.sb.call_envs("apt-get"):
            self.assertEqual(env.get("DEBIAN_FRONTEND"), "noninteractive")

    def test_present_docker_is_kept(self):
        fail_host_service(self.sb)
        self.sb.provide_docker()
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertEqual(self.docker_apt_calls(), [])
        self.assertEqual([c for c in self.sb.calls("systemctl") if c[0] == "enable" and "docker" in c], [])
        self.assertFalse((self.sb.root / "etc/apt/sources.list.d/docker.list").exists())
        self.assertIn("done=docker", self.state())

    def test_docker_without_compose_is_completed(self):
        self.sb.scenario("docker", ["compose", "version"], exit=1)
        self.sb.run(*self.ARGS)
        self.assertEqual(len(self.apt_installs()), 1)

    def test_apt_lock_is_waited_for_then_reported(self):
        self.sb.scenario("apt-get", [], contains=["install", "docker-ce"], exit=100,
                         stdout="E: Could not get lock /var/lib/dpkg/lock-frontend. "
                                "It is held by process 812 (unattended-upgr)\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("lock", result.stderr)
        self.assertIn("unattended-upgrades", result.stderr)
        for call in self.sb.calls("apt-get"):
            self.assertIn("DPkg::Lock::Timeout=300", call)
        self.assertNotIn("done=docker", self.state())

    def test_other_apt_failures_are_reported_with_their_output(self):
        self.sb.scenario("apt-get", [], contains=["install", "docker-ce"], exit=100,
                         stdout="E: Failed to fetch https://download.docker.com/ 503\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertNotIn("verrou", result.stderr)
        self.assertIn("E: Failed to fetch", self.sb.log())

    def test_docker_still_missing_after_install_is_a_failure(self):
        # same specificity as the default, declared later: it wins and installs nothing
        self.sb.scenario("apt-get", [], contains=["install", "docker-ce"], stdout="")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertNotIn("done=docker", self.state())

    def pulls(self):
        return [c for c in self.sb.calls("docker") if c[0] == "pull"]

    def test_latest_is_pulled_without_version_and_resolved_to_a_digest(self):
        self.sb.run(*self.ARGS)
        self.assertEqual(self.pulls(), [["pull", "ghcr.io/adminsyspro/vaultcenter:latest"]])
        self.assertIn(f"image={DIGEST}", self.state())
        self.assertRegex(DIGEST, r"^ghcr\.io/adminsyspro/vaultcenter@sha256:[0-9a-f]{64}$")

    def test_requested_version_is_the_tag_and_must_match_the_label(self):
        self.sb.scenario("docker", ["image", "inspect"],
                         contains=['{{index .Config.Labels "org.opencontainers.image.version"}}'],
                         stdout="0.2.0\n")
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS, "--version", "0.2.0")
        self.assertIn("Host storage service", result.stderr)  # all four steps passed
        self.assertEqual(self.pulls(), [["pull", "ghcr.io/adminsyspro/vaultcenter:0.2.0"]])
        self.assertIn("version=0.2.0", self.state())

    def test_label_version_must_match_the_request(self):
        result = self.sb.run(*self.ARGS, "--version", "0.2.0")  # the label says 0.1.0
        self.assertEqual(result.returncode, 4)
        self.assertIn("0.2.0", result.stderr)
        self.assertIn("0.1.0", result.stderr)
        self.assertNotIn("done=image", self.state())
        self.assertEqual([c for c in self.sb.calls("docker") if c[0] == "create"], [])

    def test_missing_version_is_reported(self):
        self.sb.scenario("docker", ["pull"], exit=1,
                         stdout="Error response from daemon: manifest unknown\n")
        result = self.sb.run(*self.ARGS, "--version", "9.9.9")
        self.assertEqual(result.returncode, 4)
        self.assertIn("9.9.9", result.stderr)
        self.assertNotIn("done=image", self.state())

    def test_digest_of_another_repository_is_refused(self):
        self.sb.scenario("docker", ["image", "inspect"], contains=["{{index .RepoDigests 0}}"],
                         stdout="evil.example/vaultcenter@sha256:" + "a" * 64 + "\n")
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)

    def test_payload_is_extracted_and_the_temporary_container_removed(self):
        self.sb.run(*self.ARGS)
        docker = self.sb.calls("docker")
        create = [c for c in docker if c[0] == "create"]
        self.assertEqual(len(create), 1)
        self.assertEqual(create[0][1], "--name")
        name = create[0][2]
        self.assertEqual(name, "vaultcenter-install-payload")
        self.assertEqual(create[0][3:], [DIGEST])
        dest = self.sb.root / "usr/local/share/vaultcenter/0.1.0"
        copies = [c for c in docker if c[0] == "cp"]
        self.assertEqual(copies, [
            ["cp", f"{name}:/usr/local/share/vaultcenter/host", f"{dest}/"],
            ["cp", f"{name}:/usr/share/doc/mhvtl/source.tar.gz", f"{dest}/mhvtl-source.tar.gz"]])
        self.assertTrue((dest / "host").is_dir())
        self.assertTrue((dest / "mhvtl-source.tar.gz").is_file())
        self.assertEqual([c for c in docker if c[0] == "start"], [])
        self.assertIn(["rm", "-f", name], docker)

    def test_temporary_container_is_removed_when_the_copy_fails(self):
        self.sb.scenario("docker", ["cp"], exit=1, stdout="Error: no such file\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        docker = self.sb.calls("docker")
        name = [c for c in docker if c[0] == "create"][0][2]
        self.assertIn(["rm", "-f", name], docker)
        self.assertNotIn("done=payload", self.state())

    def test_leftover_payload_container_is_removed_before_create(self):
        self.sb.run(*self.ARGS)
        docker = self.sb.calls("docker")
        name = "vaultcenter-install-payload"
        first_create = docker.index(["create", "--name", name, DIGEST])
        self.assertIn(["rm", "-f", name], docker[:first_create])
        self.assertIn(["rm", "-f", name], docker[first_create:])

    def test_payload_container_is_removed_on_sigterm_during_copy(self):
        self.sb.scenario("docker", ["cp"], block=True)
        process = self.sb.start(*self.ARGS)
        self.addCleanup(process.wait)
        self.assertTrue(self.sb.wait_blocked(), "docker cp never started")
        os.killpg(process.pid, signal.SIGTERM)
        process.communicate(timeout=20)
        self.assertEqual(process.returncode, 143)
        docker = self.sb.calls("docker")
        self.assertIn(["rm", "-f", "vaultcenter-install-payload"], docker[docker.index(
            ["cp", "vaultcenter-install-payload:/usr/local/share/vaultcenter/host",
             f"{self.sb.root}/usr/local/share/vaultcenter/0.1.0/"]):])
        self.assertIn(["logout", "ghcr.io"], docker)

    def test_registry_logout_follows_the_pull(self):
        self.sb.run(*self.ARGS)
        docker = self.sb.calls("docker")
        pull = docker.index(["pull", "ghcr.io/adminsyspro/vaultcenter:latest"])
        self.assertEqual(docker.count(["logout", "ghcr.io"]), 1)
        self.assertGreater(docker.index(["logout", "ghcr.io"]), pull)
        self.assertLess(docker.index(["logout", "ghcr.io"]),
                        [c[0] for c in docker].index("create"))

    def test_registry_logout_also_follows_a_failed_pull(self):
        self.sb.scenario("docker", ["pull"], exit=1, stdout="Error: manifest unknown\n")
        self.assertEqual(self.sb.run(*self.ARGS, "--version", "9.9.9").returncode, 4)
        docker = self.sb.calls("docker")
        self.assertGreater(docker.index(["logout", "ghcr.io"]),
                           docker.index(["pull", "ghcr.io/adminsyspro/vaultcenter:9.9.9"]))

    def test_docker_commands_use_the_installer_configuration_directory(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        config = self.sb.root / "etc/vaultcenter-install/docker"
        self.assertEqual(stat.S_IMODE(os.stat(config).st_mode), 0o700)
        envs = [e for c, e in zip(self.sb.calls("docker"), self.sb.call_envs("docker"))
                if c[0] in ("login", "pull", "create", "cp", "rm") or c[:2] == ["compose", "version"]]
        self.assertTrue(envs)
        for env in envs:
            self.assertEqual(env.get("DOCKER_CONFIG"), str(config))
        self.assertFalse((self.sb.root / "etc/vaultcenter-host").exists())

    def test_proxy_is_passed_to_apt_and_docker(self):
        proxy = "http://proxy.example:3128"
        self.sb.run(*self.ARGS, env={"HTTPS_PROXY": proxy, "NO_PROXY": "localhost"})
        for tool in ("apt-get", "curl", "docker"):
            envs = self.sb.call_envs(tool)
            self.assertTrue(envs, tool)
            self.assertTrue(any(e.get("HTTPS_PROXY") == proxy for e in envs), tool)
        dropin = (self.sb.root / "etc/systemd/system/docker.service.d/vaultcenter-proxy.conf").read_text()
        self.assertIn("[Service]", dropin)
        self.assertIn(f"Environment=HTTPS_PROXY={proxy}", dropin)
        self.assertIn("Environment=NO_PROXY=localhost", dropin)
        self.assertNotIn("HTTP_PROXY=", dropin.replace("HTTPS_PROXY=", ""))
        systemctl = self.sb.calls("systemctl")
        self.assertIn(["daemon-reload"], systemctl)
        self.assertIn(["restart", "docker"], systemctl)

    def apt_proxy_options(self, call):
        return {word.split("=", 1)[0]: word.split("=", 1)[1] for word in call if word.startswith("Acquire::")}

    def test_proxy_is_given_to_every_apt_call_explicitly(self):
        # apt reads only the lowercase variables; the script documents HTTPS_PROXY.
        cases = [({"HTTPS_PROXY": "http://proxy.example:3128"},
                  "http://proxy.example:3128", "http://proxy.example:3128"),
                 ({"HTTPS_PROXY": "http://secure.example:3129", "HTTP_PROXY": "http://plain.example:3128"},
                  "http://plain.example:3128", "http://secure.example:3129"),
                 ({"https_proxy": "http://lower.example:1"}, "http://lower.example:1", "http://lower.example:1"),
                 ({"HTTP_PROXY": "http://plain.example:3128"},
                  "http://plain.example:3128", "http://plain.example:3128")]
        for env, http, https in cases:
            with self.subTest(env):
                sb = Sandbox()
                self.addCleanup(sb.close)
                fail_host_service(sb)
                sb.run(*self.ARGS, env=env)
                calls = sb.calls("apt-get")
                self.assertEqual([next(w for w in c if w in ("update", "install")) for c in calls],
                                 ["update", "install"])
                for call in calls:
                    self.assertEqual(self.apt_proxy_options(call),
                                     {"Acquire::http::Proxy": http, "Acquire::https::Proxy": https}, call)
                    self.assertEqual(call[call.index("Acquire::http::Proxy=" + http) - 1], "-o")

    def test_no_proxy_means_no_proxy_option_for_apt(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        self.assertTrue(self.sb.calls("apt-get"))
        for call in self.sb.calls("apt-get"):
            self.assertEqual(self.apt_proxy_options(call), {})

    def test_proxy_password_never_reaches_the_log(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS, env={"HTTPS_PROXY": "http://alice:PROXYSECRET42@proxy.example:3128"})
        log = self.sb.log()
        self.assertIn("Acquire::https::Proxy=http://alice:***@proxy.example:3128", log)
        self.assertNotIn("PROXYSECRET42", log)

    def test_pull_says_when_it_begins_and_ends_without_a_terminal(self):
        for options in ((), ("--quiet",)):
            with self.subTest(options):
                sb = Sandbox()
                self.addCleanup(sb.close)
                fail_host_service(sb)
                lines = sb.run(*self.ARGS, *options).stdout.splitlines()
                begin = next(i for i, line in enumerate(lines)
                             if line.startswith("Downloading image ghcr.io/adminsyspro/vaultcenter:latest"))
                end = lines.index("Image download finished.")
                step = next(i for i, line in enumerate(lines) if line.startswith("[4/11] VaultCenter image: image 0.1.0"))
                self.assertLess(begin, end)
                self.assertLess(end, step)

    def test_pull_progress_is_shown_on_a_terminal_and_logged(self):
        self.sb.scenario("docker", ["pull"], stdout="latest: Pulling from adminsyspro/vaultcenter\n"
                                                    "0a1b2c3d4e5f: Pull complete\nStatus: Downloaded newer image\n")
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS, tty=True)
        for text in ("downloading ghcr.io/adminsyspro/vaultcenter:latest", "0a1b2c3d4e5f: Pull complete",
                     "Status: Downloaded newer image"):
            self.assertIn(text, result.stdout)
        self.assertIn("0a1b2c3d4e5f: Pull complete", self.sb.log())
        self.assertNotIn(REGISTRY_PASSWORD, result.stdout + self.sb.log())
        self.assertLess(result.stdout.index("Pull complete"), result.stdout.index("image 0.1.0"))

    def test_failed_pull_is_still_understood_on_a_terminal(self):
        self.sb.scenario("docker", ["pull"], exit=1, stdout="Error response from daemon: manifest unknown\n")
        result = self.sb.run(*self.ARGS, "--version", "9.9.9", tty=True)
        self.assertEqual(result.returncode, 4, result.stdout)
        self.assertIn("version 9.9.9 does not exist", result.stdout)
        self.assertIn("manifest unknown", self.sb.log())

    def test_no_proxy_means_no_dropin_and_no_restart(self):
        self.sb.run(*self.ARGS)
        self.assertFalse((self.sb.root / "etc/systemd/system/docker.service.d").exists())
        self.assertNotIn(["restart", "docker"], self.sb.calls("systemctl"))

    def test_lowercase_proxy_variable_is_honoured(self):
        self.sb.run(*self.ARGS, env={"https_proxy": "http://p:1"})
        dropin = (self.sb.root / "etc/systemd/system/docker.service.d/vaultcenter-proxy.conf").read_text()
        self.assertIn("Environment=HTTPS_PROXY=http://p:1", dropin)

    def resume(self, *done):
        state = self.sb.root / "var/lib/vaultcenter-install/state"
        state.parent.mkdir(parents=True)
        state.write_text("".join(f"done={name}\n" for name in done)
                         + f"image={DIGEST}\nversion=0.1.0\n")

    def test_resume_skips_finished_steps_but_asks_credentials_again(self):
        self.sb.provide_docker()
        self.resume("preflight", "credentials", "docker", "image")
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertEqual([c for c in self.sb.calls("docker") if c[0] in ("pull", "login")], [])
        self.assertEqual(self.docker_apt_calls(), [])
        create = [c for c in self.sb.calls("docker") if c[0] == "create"]
        self.assertEqual(create[0][3:], [DIGEST])  # the image of the state file
        self.assertIn("done=payload", self.state())

    def test_resume_before_the_image_logs_in_again(self):
        self.sb.provide_docker()
        self.resume("preflight", "credentials", "docker")
        self.sb.run(*self.ARGS)
        self.assertEqual(len(self.site_calls()), 1)
        self.assertEqual(len([c for c in self.sb.calls("docker") if c[0] == "login"]), 1)
        self.assertEqual(len(self.pulls()), 1)
        self.assertEqual(self.docker_apt_calls(), [])


class KernelModuleTest(unittest.TestCase):
    ARGS = ("--token", TOKEN, "--yes")
    KERNEL = "6.1.0-test-amd64"

    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)
        self.version = "vc-" + self.sb.mhvtl_archive_sha256[:12]

    def state(self):
        path = self.sb.root / "var/lib/vaultcenter-install/state"
        return path.read_text().splitlines() if path.exists() else []

    def dkms(self, verb=None):
        return [c for c in self.sb.calls("dkms") if verb is None or c[0] == verb]

    def installed(self):
        self.sb.scenario(
            "dkms", ["status"], stdout=f"mhvtl/{self.version}, {self.KERNEL}, x86_64: installed\n")

    def test_dkms_builds_and_installs_for_the_running_kernel(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        steps = [c for c in self.dkms() if c[0] in ("add", "build", "install")]
        base = ["-m", "mhvtl", "-v", self.version]
        self.assertEqual([c[:5] for c in steps], [
            ["add", *base], ["build", *base], ["install", *base]])
        for call in steps[1:]:
            self.assertEqual(call[5:7], ["-k", self.KERNEL])
        source = self.sb.root / f"usr/src/mhvtl-{self.version}"
        self.assertTrue((source / "kernel/Makefile").is_file())
        conf = (source / "dkms.conf").read_text()
        self.assertIn(f'PACKAGE_VERSION="{self.version}"', conf)
        self.assertNotIn("@VERSION@", conf)
        self.assertIn("done=kernel_module", self.state())

    def test_packages_come_first_with_the_apt_helper(self):
        self.sb.run(*self.ARGS)
        installs = [c for c in self.sb.calls("apt-get") if "dkms" in c]
        self.assertEqual(len(installs), 1)
        self.assertTrue({"install", "-y", "dkms", "build-essential",
                         f"linux-headers-{self.KERNEL}"} <= set(installs[0]))
        self.assertIn("DPkg::Lock::Timeout=300", installs[0])
        names = [c[0] for c in self.sb.calls()]
        self.assertLess(len(names) - 1 - names[::-1].index("apt-get"), names.index("dkms"))

    def headers_packages(self):
        install = next(c for c in self.sb.calls("apt-get") if "dkms" in c)
        return [word for word in install if word.startswith("linux-headers-")]

    def test_headers_package_of_the_flavour_follows_the_kernel_upgrades(self):
        cases = {"6.1.0-test-amd64": "linux-headers-amd64",
                 "6.1.0-28-cloud-amd64": "linux-headers-cloud-amd64",
                 "6.12.48+deb13-cloud-amd64": "linux-headers-cloud-amd64",
                 "6.1.0-28-rt-amd64": "linux-headers-rt-amd64"}
        for kernel, package in cases.items():
            with self.subTest(kernel):
                sb = Sandbox()
                self.addCleanup(sb.close)
                self.sb = sb
                sb.scenario("uname", ["-r"], stdout=kernel + "\n")
                fail_host_service(sb)
                result = sb.run(*self.ARGS)
                self.assertEqual(self.headers_packages(), [f"linux-headers-{kernel}", package])
                self.assertNotIn("saveur", result.stderr)

    def test_unknown_kernel_flavour_keeps_the_versioned_package_and_warns(self):
        self.sb.scenario("uname", ["-r"], stdout="6.8.12-4-pve\n")
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(self.headers_packages(), ["linux-headers-6.8.12-4-pve"])
        self.assertIn("Warning", result.stderr)
        self.assertIn("pve", result.stderr)
        self.assertIn("done=kernel_module", self.state())
        self.assertIn("WARNING", self.sb.log())

    def other_kernel(self, name="6.1.0-next-amd64"):
        (self.sb.root / "lib/modules" / name / "build").mkdir(parents=True)
        return name

    def test_module_is_built_for_every_kernel_that_has_its_headers(self):
        older, newer = self.other_kernel("6.1.0-0old-amd64"), self.other_kernel()
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS)
        for verb in ("build", "install"):
            self.assertEqual([c[c.index("-k") + 1] for c in self.dkms(verb)], [self.KERNEL, older, newer])
        self.assertEqual(len(self.dkms("add")), 1)
        self.assertIn("2 other installed kernel(s)", result.stdout)
        self.assertIn("done=kernel_module", self.state())

    def test_failure_for_a_kernel_that_is_not_running_is_a_warning(self):
        other = self.other_kernel()
        self.sb.scenario("dkms", ["build"], contains=[other], exit=10)
        fail_host_service(self.sb)
        result = self.sb.run(*self.ARGS)
        self.assertIn("done=kernel_module", self.state())
        self.assertIn("Warning", result.stderr)
        self.assertIn(other, result.stderr)
        self.assertIn(f"dkms install -m mhvtl -v {self.version} -k {other}", result.stderr)
        self.assertEqual([c[c.index("-k") + 1] for c in self.dkms("install")], [self.KERNEL])
        self.assertIn(["mhvtl", "opts=0"], self.sb.calls("modprobe"))

    def test_failure_for_the_running_kernel_stops_before_the_others(self):
        self.other_kernel()
        self.sb.scenario("dkms", ["build"], contains=[self.KERNEL], exit=10)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertEqual(len(self.dkms("build")), 1)
        self.assertNotIn("done=kernel_module", self.state())

    def test_a_kernel_added_since_gets_the_module_on_resume(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        state = self.sb.root / "var/lib/vaultcenter-install/state"
        state.write_text(state.read_text().replace("done=kernel_module\n", ""))
        other = self.other_kernel()
        self.sb.scenario("dkms", ["status"], contains=[self.KERNEL],
                         stdout=f"mhvtl/{self.version}, {self.KERNEL}, x86_64: installed\n")
        self.sb.scenario("dkms", ["add"], exit=3, stdout="Error! DKMS tree already contains: mhvtl\n")
        self.sb.reset_calls()
        self.sb.run(*self.ARGS)
        self.assertEqual([c[c.index("-k") + 1] for c in self.dkms("build")], [other])
        self.assertIn("done=kernel_module", self.state())

    def test_boot_guard_unit_is_installed_and_enabled(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        unit = self.sb.root / "etc/systemd/system/vaultcenter-mhvtl.service"
        shipped = self.sb.root / "usr/local/share/vaultcenter/0.1.0/host/vaultcenter-mhvtl.service"
        self.assertEqual(unit.read_text(), shipped.read_text())
        self.assertEqual(stat.S_IMODE(unit.stat().st_mode), 0o644)
        systemctl = [c for c in self.sb.calls("systemctl") if c[0] != "is-active"]
        self.assertEqual(systemctl[-2:], [["daemon-reload"], ["enable", "vaultcenter-mhvtl.service"]])
        self.assertIn("done=kernel_module", self.state())

    def test_boot_guard_that_cannot_be_enabled_fails_the_step(self):
        self.sb.scenario("systemctl", ["enable", "vaultcenter-mhvtl.service"], exit=1)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("vaultcenter-mhvtl.service", result.stderr)
        self.assertNotIn("done=kernel_module", self.state())

    def test_modules_are_loaded_with_tracing_off(self):
        self.sb.run(*self.ARGS)
        self.assertEqual(self.sb.calls("modprobe"), [["mhvtl", "opts=0"], ["sg"]])
        names = [c[0] for c in self.sb.calls()]
        self.assertLess(max(i for i, n in enumerate(names) if n == "dkms"), names.index("modprobe"))

    def test_persistent_settings_are_installed(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        host = self.sb.root / "usr/local/share/vaultcenter/0.1.0/host"
        for name, target in (("vaultcenter-mhvtl.conf", "etc/modprobe.d/vaultcenter-mhvtl.conf"),
                             ("99-vaultcenter-console.conf", "etc/sysctl.d/99-vaultcenter-console.conf"),
                             ("99-vaultcenter.rules", "etc/udev/rules.d/99-vaultcenter.rules")):
            path = self.sb.root / target
            self.assertEqual(path.read_text(), (host / name).read_text())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o644, target)
        load = self.sb.root / "etc/modules-load.d/vaultcenter.conf"
        self.assertEqual(load.read_text().split(), ["mhvtl", "sg"])
        self.assertEqual(stat.S_IMODE(load.stat().st_mode), 0o644)
        self.assertEqual(self.sb.calls("sysctl"), [
            ["--load", f"{self.sb.root}/etc/sysctl.d/99-vaultcenter-console.conf"]])
        udev = self.sb.calls("udevadm")
        self.assertEqual(udev, [["control", "--reload-rules"],
                                ["trigger", "--subsystem-match=scsi_generic"]])

    def test_the_full_fingerprint_is_recorded(self):
        self.sb.run(*self.ARGS)
        recorded = self.sb.root / "var/lib/vaultcenter-install/mhvtl.sha256"
        self.assertEqual(recorded.read_text().strip(), self.sb.mhvtl_archive_sha256)

    def test_foreign_loaded_module_is_refused(self):
        (self.sb.root / "sys/module/mhvtl").mkdir(parents=True)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 3)
        self.assertIn("another origin", result.stderr)
        self.assertEqual(self.dkms("build") + self.dkms("add") + self.dkms("install"), [])
        self.assertEqual(self.sb.calls("modprobe"), [])
        self.assertNotIn("done=kernel_module", self.state())

    def test_our_own_loaded_module_is_kept(self):
        fail_host_service(self.sb)
        (self.sb.root / "sys/module/mhvtl").mkdir(parents=True)
        self.installed()
        result = self.sb.run(*self.ARGS)
        self.assertNotIn("autre origine", result.stderr)
        self.assertEqual(self.dkms("build"), [])
        self.assertEqual(self.sb.calls("modprobe"), [["sg"]])
        self.assertIn("done=kernel_module", self.state())

    def test_same_sources_are_not_rebuilt(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        self.assertEqual(len(self.dkms("build")), 1)
        state = self.sb.root / "var/lib/vaultcenter-install/state"
        state.write_text(state.read_text().replace("done=kernel_module\n", ""))
        self.installed()
        self.sb.reset_calls()
        self.sb.run(*self.ARGS)
        self.assertEqual(self.dkms("build"), [])
        self.assertEqual(self.dkms("add"), [])
        self.assertIn(["mhvtl", "opts=0"], self.sb.calls("modprobe"))
        self.assertIn("done=kernel_module", self.state())

    def test_a_finished_step_is_skipped_on_resume(self):
        fail_host_service(self.sb)
        self.sb.run(*self.ARGS)
        self.assertIn("done=kernel_module", self.state())
        self.sb.reset_calls()
        self.sb.run(*self.ARGS)
        self.assertEqual(self.sb.calls("dkms"), [])
        self.assertEqual(self.sb.calls("modprobe"), [])

    def test_build_failure_shows_the_dkms_log_path(self):
        self.sb.scenario("dkms", ["build"], exit=10, stdout="Error! Bad return status for module build\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn(f"{self.sb.root}/var/lib/dkms/mhvtl/{self.version}/build/make.log", result.stderr)
        self.assertEqual(self.dkms("install"), [])
        self.assertEqual(self.sb.calls("modprobe"), [])
        self.assertNotIn("done=kernel_module", self.state())

    def apt_sequence(self):
        return [next(w for w in c if w in ("update", "install"))
                for c in self.sb.calls("apt-get") if "update" in c or "install" in c]

    def test_present_docker_still_refreshes_the_apt_lists_once_before_the_install(self):
        self.sb.provide_docker()
        self.sb.run(*self.ARGS)
        self.assertEqual(self.apt_sequence(), ["update", "install"])

    def test_apt_lists_are_refreshed_again_once_the_docker_repository_is_added(self):
        # The lists the preflight refreshed do not know docker-ce: without a second
        # refresh, apt would not find the packages of the repository just added.
        self.sb.run(*self.ARGS)
        self.assertEqual(self.apt_sequence(), ["update", "update", "install", "install"])
        calls = self.sb.calls("apt-get")
        self.assertIn("docker-ce", calls[2])
        self.assertIn("dkms", calls[3])

    def test_trees_are_passed_to_dkms_only_under_a_test_root(self):
        self.sb.run(*self.ARGS)
        self.assertTrue(self.dkms("add"))
        for call in self.dkms("add"):
            self.assertIn("--sourcetree", call)
            self.assertEqual(call[call.index("--sourcetree") + 1], f"{self.sb.root}/usr/src")
            self.assertEqual(call[call.index("--dkmstree") + 1], f"{self.sb.root}/var/lib/dkms")


STEPS = ("preflight", "credentials", "docker", "image", "payload", "kernel_module",
         "host_service", "configuration", "administrator", "start", "summary")
LABELS = ("Preflight checks", "Registry credentials", "Docker", "VaultCenter image",
          "Host files", "mhvtl kernel module", "Host storage service",
          "Configuration", "Administrator account", "Startup", "Summary")
HEALTH_FORMAT = "{{.State.Health.Status}}"
FINGERPRINT = ":".join(["AB", "CD", "EF", "01"] * 8)
PASSWORD_PATTERN = r"[A-Za-z0-9]{24}"


class FreshInstallTest(unittest.TestCase):
    """The last five steps of a fresh installation, and resuming any of the eleven."""
    ARGS = ("--token", TOKEN, "--yes")
    HOST = "etc/vaultcenter-host"

    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)
        self.sb.provide_docker()

    def path(self, relative):
        return self.sb.root / relative

    def state(self):
        path = self.path("var/lib/vaultcenter-install/state")
        return path.read_text().splitlines() if path.exists() else []

    def done(self):
        return [line[5:] for line in self.state() if line.startswith("done=")]

    def mode(self):
        return re.findall(r" mode=(\w+)$", self.sb.log(), re.M)[-1]

    def install(self, *extra):
        result = self.sb.run(*self.ARGS, *extra)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def mode_of(self, relative):
        return stat.S_IMODE(os.stat(self.path(relative)).st_mode)

    def compose_call(self, verb):
        return [c for c in self.sb.calls("docker") if c[:1] == ["compose"] and verb in c]

    def password_from_stdin(self):
        sent = self.sb.stdin("docker")[-1]
        self.assertRegex(sent, r"^" + PASSWORD_PATTERN + r"\n$")
        return sent.strip()

    def test_fresh_install_runs_every_step_in_order(self):
        for release in ("12", "13"):
            with self.subTest(release):
                sb = Sandbox(release)
                self.addCleanup(sb.close)
                sb.provide_docker()
                result = sb.run(*self.ARGS, "--quiet")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                lines = [line for line in result.stdout.splitlines() if re.match(r"^\[\d+/11\] ", line)]
                self.assertEqual([re.match(r"^\[(\d+)/11\] (.+?): ", line).group(1, 2) for line in lines],
                                 [(str(n + 1), label) for n, label in enumerate(LABELS)])
                self.assertEqual(sb.calls("docker")[-1][:1], ["inspect"])  # the health wait is last

    def test_host_installer_receives_the_digest_and_isolated(self):
        self.install()
        compose = json.loads(self.path(self.HOST + "/compose.json").read_text())
        self.assertEqual(compose["services"]["appliance"]["image"], DIGEST)
        self.assertTrue(self.path(self.HOST + "/isolated-mode").is_file())
        self.assertTrue(json.loads(self.path(self.HOST + "/storage.json").read_text())["isolated"])
        enabled = [c for c in self.sb.calls("systemctl") if c[:1] == ["enable"]]
        self.assertEqual([c[-1] for c in enabled],
                         ["vaultcenter-mhvtl.service", "vaultcenter-storage-mounts.service",
                          "vaultcenter-storage-host.service"])
        # install.py reads a Compose source whose binds point at the final configuration.
        config = [c for c in self.compose_call("config")][0]
        source = Path(config[config.index("-f") + 1])
        self.assertEqual(source.name, "compose.yaml")
        text = source.read_text()
        self.assertIn("source: /etc/vaultcenter-host/config/cold-vtl\n", text)
        self.assertIn("source: /etc/vaultcenter-host/config/targets.conf\n", text)
        self.assertNotIn("./config", text)
        self.assertNotIn(str(self.sb.root), text)  # consumed by Docker: logical paths
        for call, env in zip(self.sb.calls("docker"), self.sb.call_envs("docker")):
            if call[:1] in (["compose"], ["volume"]):
                self.assertEqual(env.get("DOCKER_CONFIG"), f"{self.sb.root}/etc/vaultcenter-install/docker", call)
        self.assertEqual(self.done(), [])

    def test_payload_ships_the_guide_the_messages_point_to(self):
        self.install()
        shipped = self.path(LOCAL_DOC.lstrip("/"))
        self.assertEqual(shipped.read_text(), (Path(__file__).parents[2] / "docs/install.md").read_text())

    def test_resume_with_another_version_is_refused(self):
        fail_host_service(self.sb)
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)
        self.assertIn("version=0.1.0", self.state())
        before = self.state()
        self.sb.reset_scenario()
        self.sb.reset_calls()
        result = self.sb.run(*self.ARGS, "--version", "0.2.0")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        for text in ("0.1.0", "0.2.0", "without --version", "/var/lib/vaultcenter-install/state", LOCAL_DOC,
                     "Nothing has been changed"):
            self.assertIn(text, result.stderr)
        self.assertEqual(self.state(), before)
        self.assertEqual([c for c in self.sb.calls() if c[0] != "id" and c[:2] != ["docker", "ps"]], [])
        # The recorded version, or none, goes on with the installation.
        for options in (("--version", "0.1.0"), ()):
            with self.subTest(options):
                sb = Sandbox()
                self.addCleanup(sb.close)
                sb.provide_docker()
                fail_host_service(sb)
                self.assertEqual(sb.run(*self.ARGS).returncode, 4)
                sb.reset_scenario()
                self.assertEqual(sb.run(*self.ARGS, *options).returncode, 0)

    def test_host_installer_stopped_after_compose_json_is_explained_never_repaired(self):
        # install.py wrote compose.json and its volumes, then stopped: it refuses
        # to run again, and its volumes are no longer empty.
        self.sb.scenario("systemctl", ["enable", "--now", "vaultcenter-storage-mounts.service"], exit=1)
        first = self.sb.run(*self.ARGS)
        self.assertEqual(first.returncode, 4, first.stdout + first.stderr)
        compose = self.path(self.HOST + "/compose.json")
        self.assertTrue(compose.is_file())
        self.assertNotIn("host_service", self.done())
        self.assertIn("installing=1", self.state())
        volume = self.path("var/lib/docker/volumes/vaultcenter_state/_data")
        kept = {str(p.relative_to(self.sb.root)) for p in self.sb.root.rglob("*")
                if "vaultcenter-host" in str(p) or "volumes" in str(p)}
        self.assertTrue(any(volume.iterdir()))
        self.sb.reset_scenario()
        self.sb.reset_calls()
        second = self.sb.run(*self.ARGS)
        self.assertEqual(second.returncode, 4, second.stdout + second.stderr)
        for text in ("/etc/vaultcenter-host/compose.json exists", "Nothing has been deleted",
                     "the /etc/vaultcenter-host directory", "vaultcenter_state and vaultcenter_media",
                     "Deleting them is your decision", "rm -r /etc/vaultcenter-host",
                     "docker volume rm vaultcenter_state vaultcenter_media", LOCAL_DOC):
            self.assertIn(text, second.stderr)
        self.assertEqual({str(p.relative_to(self.sb.root)) for p in self.sb.root.rglob("*")
                          if "vaultcenter-host" in str(p) or "volumes" in str(p)}, kept)
        # The host installer was not run again, and nothing was removed.
        self.assertEqual(self.compose_call("config"), [])
        self.assertEqual([c for c in self.sb.calls("docker") if c[:1] == ["volume"]], [])
        self.assertNotIn("host_service", self.done())

    def test_existing_administrator_is_done_with_an_unknown_password(self):
        # create-user succeeded in an earlier run that died before recording it.
        self.sb.scenario("docker", ["compose", "-f"], contains=["create-user"], exit=1,
                         stderr='Error: "username already exists"\n')
        result = self.install()
        self.assertIn("admin account already present", result.stdout)
        self.assertRegex(result.stdout, r"Password:\s*unknown")
        self.assertIn(LOCAL_DOC, result.stdout)
        self.assertIn("The administrator account", result.stdout)
        password = self.password_from_stdin()
        self.assertNotIn(password, result.stdout + self.sb.log())

    def test_another_create_user_failure_is_still_a_failure(self):
        self.sb.scenario("docker", ["compose", "-f"], contains=["create-user"], exit=1,
                         stderr="Error: authentication operation failed\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertNotIn("administrator", self.done())

    def test_start_failure_says_the_new_password_will_not_be_shown(self):
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="starting\n")
        first = self.sb.run(*self.ARGS)
        self.assertEqual(first.returncode, 4)
        self.assertIn("will not be shown when resuming", first.stderr)
        self.assertIn("The administrator account", first.stderr)
        self.assertIn(LOCAL_DOC, first.stderr)
        self.assertNotIn(self.password_from_stdin(), first.stdout + first.stderr)
        # On the resumed run there is no new password to lose.
        second = self.sb.run(*self.ARGS)
        self.assertEqual(second.returncode, 4)
        self.assertNotIn("will not be shown", second.stderr)

    def test_host_installer_leftover_is_reported_never_deleted(self):
        leftover = self.path(self.HOST)
        leftover.mkdir(parents=True)
        (leftover / "partial").write_text("x")
        state = self.path("var/lib/vaultcenter-install/state")
        state.parent.mkdir(parents=True)
        state.write_text("".join(f"done={n}\n" for n in STEPS[:6]) + f"image={DIGEST}\nversion=0.1.0\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("/etc/vaultcenter-host", result.stderr)
        self.assertIn("delete the directory", result.stderr)
        self.assertTrue((leftover / "partial").is_file())
        self.assertEqual(self.compose_call("config"), [])

    def test_configuration_files_and_modes(self):
        self.install()
        config = self.HOST + "/config"
        for directory in (config, config + "/cold-vtl", config + "/cold-vtl/tls",
                          config + "/cold-vtl/maintenance"):
            self.assertEqual(self.mode_of(directory), 0o700, directory)
        auth = self.path(config + "/cold-vtl/auth.env")
        self.assertEqual(self.mode_of(config + "/cold-vtl/auth.env"), 0o600)
        lines = auth.read_text().splitlines()
        self.assertIn("COLD_VTL_PUBLIC_URL=https://vaultcenter-test.example.test:8443", lines)
        self.assertIn("COLD_VTL_LISTEN=0.0.0.0:8443", lines)
        self.assertIn("COLD_VTL_TLS_CERT=/etc/cold-vtl/tls/fullchain.pem", lines)
        self.assertEqual(self.mode_of(config + "/cold-vtl/tls/privkey.pem"), 0o600)
        self.assertTrue(self.path(config + "/cold-vtl/tls/fullchain.pem").is_file())
        targets = self.path(config + "/targets.conf")
        self.assertEqual(self.mode_of(config + "/targets.conf"), 0o600)
        content = targets.read_text().splitlines()
        self.assertEqual(len(content), 1)
        self.assertTrue(content[0].startswith("#"))
        # Without the runtime pool the appliance refuses to start in isolated mode.
        self.assertEqual(self.mode_of(config + "/cold-vtl/vtl-runtime.json"), 0o600)
        pool = json.loads(self.path(config + "/cold-vtl/vtl-runtime.json").read_text())
        self.assertEqual(pool["bindings"], {})
        self.assertEqual(len(pool["resources"]), 4)
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "cold-vtl"))
        from vtl_runtime import Registry
        Registry(self.sb.root, pool).validate_pool()

    def test_a_resumed_installation_keeps_the_runtime_pool(self):
        # A run that stops at startup, a pool edited by the operator, then a resume.
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="starting\n")
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)
        pool = self.path(self.HOST + "/config/cold-vtl/vtl-runtime.json")
        edited = '{"resources": [], "bindings": {"x": 1}}\n'
        pool.write_text(edited)
        self.sb.reset_scenario()
        self.install()
        self.assertEqual(pool.read_text(), edited)

    def test_certificate_names_the_host_and_its_addresses(self):
        self.install()
        request = self.sb.calls("openssl")[0]
        tls = f"{self.sb.root}/etc/vaultcenter-host/config/cold-vtl/tls"
        self.assertEqual(request[:3], ["req", "-x509", "-newkey"])
        self.assertEqual(request[request.index("-newkey") + 1], "rsa:2048")
        self.assertIn("-nodes", request)
        self.assertEqual(request[request.index("-days") + 1], "825")
        self.assertEqual(request[request.index("-subj") + 1], "/CN=vaultcenter-test.example.test")
        self.assertEqual(request[request.index("-addext") + 1],
                         "subjectAltName=DNS:vaultcenter-test.example.test,IP:10.0.0.5,IP:2001:db8::5")
        self.assertEqual(request[request.index("-keyout") + 1], tls + "/privkey.pem")
        self.assertEqual(request[request.index("-out") + 1], tls + "/fullchain.pem")

    def test_an_existing_certificate_is_never_overwritten(self):
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="starting\n")
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)
        tls = self.path(self.HOST + "/config/cold-vtl/tls")
        (tls / "fullchain.pem").write_text("MINE")
        state = self.path("var/lib/vaultcenter-install/state")
        state.write_text("".join(line + "\n" for line in self.state() if line != "done=configuration"))
        self.sb.reset_scenario()
        self.sb.reset_calls()
        self.install()
        self.assertEqual((tls / "fullchain.pem").read_text(), "MINE")
        self.assertEqual(self.sb.calls("openssl")[:1], [["x509", "-noout", "-fingerprint", "-sha256", "-in",
                                                         str(tls / "fullchain.pem")]])

    def test_host_without_a_full_name_uses_its_address(self):
        for name, scenario in (("fails", {"exit": 1}), ("empty", {"stdout": "\n"}),
                               ("no dot", {"stdout": "vaultcenter-test\n"}),
                               ("localhost", {"stdout": "localhost\n"}),
                               ("localdomain", {"stdout": "localhost.localdomain\n"})):
            with self.subTest(name):
                sb = Sandbox()
                self.addCleanup(sb.close)
                sb.provide_docker()
                sb.scenario("hostname", ["-f"], **scenario)
                result = sb.run(*self.ARGS)
                self.assertEqual(result.returncode, 0, result.stderr)
                request = sb.calls("openssl")[0]
                self.assertEqual(request[request.index("-subj") + 1], "/CN=10.0.0.5")
                self.assertEqual(request[request.index("-addext") + 1],
                                 "subjectAltName=IP:10.0.0.5,IP:2001:db8::5")
                auth = (sb.root / self.HOST / "config/cold-vtl/auth.env").read_text()
                self.assertIn("COLD_VTL_PUBLIC_URL=https://10.0.0.5:8443\n", auth)
                self.assertIn("IP address", result.stdout)
                self.assertIn("10.0.0.5", result.stdout)

    def test_a_named_host_does_not_get_the_address_sentence(self):
        self.assertNotIn("adresse IP", self.install().stdout)

    def test_admin_password_is_shown_once_and_never_logged(self):
        result = self.install()
        password = self.password_from_stdin()
        self.assertEqual(result.stdout.count(password), 1)
        self.assertNotIn(password, result.stderr)
        self.assertNotIn(password, self.sb.log())
        for call in self.sb.calls():
            self.assertFalse(any(password in word for word in call), call)
        self.assertEqual(self.sb.stdin("docker")[-1], password + "\n")
        for file in self.sb.root.rglob("*"):
            if file.is_file() and not file.is_symlink():
                self.assertNotIn(password.encode(), file.read_bytes(), str(file))
        create = [c for c in self.sb.calls("docker") if "create-user" in c]
        self.assertEqual(create, [["compose", "-f", f"{self.sb.root}/etc/vaultcenter-host/compose.json",
                                   "run", "--rm", "-T", "--entrypoint", "/usr/local/bin/cold-vtld",
                                   "appliance", "create-user", "admin", "--role", "admin"]])

    def test_each_installation_draws_a_new_password(self):
        first = self.install()
        other = Sandbox()
        self.addCleanup(other.close)
        other.provide_docker()
        second = other.run(*self.ARGS)
        self.assertNotEqual(re.search(r"Password:\s*(\S+)", first.stdout).group(1),
                            re.search(r"Password:\s*(\S+)", second.stdout).group(1))

    def test_health_is_awaited_and_the_version_confirmed(self):
        self.install()
        up = self.compose_call("up")
        self.assertEqual(up, [["compose", "-f", f"{self.sb.root}/etc/vaultcenter-host/compose.json",
                               "up", "-d"]])
        health = [c for c in self.sb.calls("docker") if HEALTH_FORMAT in c]
        self.assertEqual(health, [["inspect", "--format", HEALTH_FORMAT, "vaultcenter-appliance-1"]])
        probes = [c for c in self.sb.calls("curl") if "https://127.0.0.1:8443/health" in c]
        self.assertEqual(len(probes), 1)
        self.assertIn("-fsSk", probes[0])
        self.assertLess(self.sb.calls().index(["docker", *health[0]]),
                        self.sb.calls().index(["curl", *probes[0]]))

    def test_health_is_read_on_the_loopback_whatever_the_proxy(self):
        # sudo -E with HTTPS_PROXY: the proxy cannot reach this machine's 127.0.0.1.
        for variable in ("HTTPS_PROXY", "https_proxy"):
            with self.subTest(variable):
                sb = Sandbox()
                self.addCleanup(sb.close)
                sb.provide_docker()
                result = sb.run(*self.ARGS, env={variable: "http://proxy.example:3128"})
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                probe = next(c for c in sb.calls("curl") if "https://127.0.0.1:8443/health" in c)
                self.assertEqual(probe[probe.index("--noproxy") + 1], "*")
                # The other calls do go through the proxy.
                site = next(c for c in sb.calls("curl") if VALIDATE_URL in c)
                self.assertNotIn("--noproxy", site)

    def test_an_unhealthy_appliance_is_polled_then_reported(self):
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="starting\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        health = [c for c in self.sb.calls("docker") if HEALTH_FORMAT in c]
        self.assertEqual(len(health), 3)  # the test limit
        self.assertEqual([c for c in self.sb.calls("curl") if "https://127.0.0.1:8443/health" in c], [])
        self.assertIn("compose -f /etc/vaultcenter-host/compose.json logs --tail=100", result.stderr)
        self.assertIn("run the same installation command again", result.stderr)

    def test_a_wrong_version_is_a_failure(self):
        self.sb.scenario("curl", [], contains=["https://127.0.0.1:8443/health"],
                         stdout=json.dumps({"status": "ok", "version": "9.9.9"}))
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertIn("9.9.9", result.stderr)
        self.assertIn("0.1.0", result.stderr)
        self.assertIn("run the same installation command again", result.stderr)

    def test_unhealthy_start_keeps_everything_in_place(self):
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="unhealthy\n")
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4)
        self.assertEqual(self.done(), list(STEPS[:9]))
        self.assertTrue(self.path(self.HOST + "/compose.json").is_file())
        self.assertTrue(self.path(self.HOST + "/config/cold-vtl/auth.env").is_file())
        removing = [c for c in self.sb.calls("docker")
                    if any(word in c for word in ("down", "stop", "kill", "rm"))
                    and c[:1] != ["rm"]]
        self.assertEqual([c for c in removing if "vaultcenter-install-payload" not in c], [])
        self.assertNotIn("Password", result.stdout)

    def test_resume_skips_finished_steps(self):
        def fail(sb, step):
            sb.scenario(*FAILURES[step][0], **FAILURES[step][1])
        for index, step in enumerate(STEPS):
            with self.subTest(step):
                sb = Sandbox()
                self.addCleanup(sb.close)
                if step != "docker":
                    sb.provide_docker()
                fail(sb, step)
                first = sb.run(*self.ARGS)
                expected_code = 3 if step == "preflight" else 4
                self.assertEqual(first.returncode, expected_code, first.stdout + first.stderr)
                state = sb.root / "var/lib/vaultcenter-install/state"
                done = [line[5:] for line in state.read_text().splitlines()
                        if line.startswith("done=")] if state.exists() else []
                self.assertEqual(done, list(STEPS[:index]))
                sb.reset_scenario()
                sb.reset_calls()
                second = sb.run(*self.ARGS)
                self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
                for finished in STEPS[:index]:
                    self.assertEqual(
                        [c for c in sb.calls() if RAN[finished](c)], [], f"{finished} ran again")
                if index > STEPS.index("administrator"):
                    self.assertIn(LOCAL_DOC, second.stdout)
                else:
                    self.assertRegex(second.stdout, PASSWORD_PATTERN)

    def test_resume_does_not_create_a_second_administrator(self):
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="starting\n")
        first = self.sb.run(*self.ARGS)
        self.assertEqual(first.returncode, 4)
        password = self.password_from_stdin()
        self.sb.reset_scenario()
        self.sb.reset_calls()
        second = self.install()
        self.assertEqual([c for c in self.sb.calls("docker") if "create-user" in c], [])
        self.assertEqual(self.sb.stdin("docker"), [])
        self.assertNotIn(password, second.stdout)
        self.assertIn(LOCAL_DOC, second.stdout)
        self.assertNotIn(password, self.sb.log())

    def test_final_box_lists_url_account_fingerprint_and_log(self):
        result = self.install()
        out = result.stdout
        self.assertIn("https://vaultcenter-test.example.test:8443", out)
        self.assertRegex(out, r"Account:\s*admin")
        self.assertRegex(out, r"Password:\s*" + PASSWORD_PATTERN)
        self.assertIn(FINGERPRINT, out)
        self.assertIn("SHA-256", out)
        self.assertIn("console", out)
        self.assertIn("/var/log/vaultcenter-install.log", out)
        fingerprint = [c for c in self.sb.calls("openssl") if c[:1] == ["x509"]]
        self.assertEqual(fingerprint, [["x509", "-noout", "-fingerprint", "-sha256", "-in",
                                        f"{self.sb.root}/etc/vaultcenter-host/config/cold-vtl/tls/fullchain.pem"]])

    def test_a_finished_installation_leaves_no_step_record(self):
        self.install()
        self.assertEqual(self.done(), [])
        self.assertNotIn("installing=1", self.state())
        self.assertIn(f"image={DIGEST}", self.state())
        self.sb.run(*self.ARGS)
        self.assertEqual(self.mode(), "update")

    def test_an_install_interrupted_after_the_host_service_is_a_resume(self):
        self.sb.scenario("docker", ["inspect", "--format", HEALTH_FORMAT], stdout="starting\n")
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)
        self.assertTrue(self.path(self.HOST + "/compose.json").is_file())
        self.assertIn("installing=1", self.state())
        self.sb.reset_scenario()
        self.install()
        self.assertEqual(self.mode(), "resume")

    def test_a_failed_update_does_not_turn_the_next_run_into_a_resume(self):
        self.install()
        self.sb.scenario("docker", ["pull"], exit=1)
        self.assertEqual(self.sb.run(*self.ARGS).returncode, 4)
        self.assertEqual(self.mode(), "update")
        self.assertEqual(self.done(), [])
        self.assertNotIn("installing=1", self.state())
        self.sb.reset_scenario()
        self.sb.run(*self.ARGS)
        self.assertEqual(self.mode(), "update")
        self.assertEqual(self.done(), [])
        self.assertNotIn("installing=1", self.state())


UPDATE_LABELS = ("Preflight checks", "Registry credentials", "Docker", "VaultCenter image",
                 "Host files", "Appliance update", "Summary")
BACKUP = "/var/lib/vaultcenter-host/backups/20261113-120000-0.0.9"
GUARD_REASON = "No maintenance guard answers for VTL 1: run again with --yes."
RESULT_EXIT = {"updated": 0, "unchanged": 0, "refused": 3, "rolled_back": 5, "rollback_failed": 6}


class UpdateModeTest(unittest.TestCase):
    """An installed appliance: pull the target image, hand over to its updater."""
    ARGS = ("--token", TOKEN, "--yes")
    HOST = "etc/vaultcenter-host"

    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)
        self.sb.provide_docker()
        compose = self.sb.root / self.HOST / "compose.json"
        compose.parent.mkdir(parents=True)
        compose.write_text("{}")
        self.payload = f"{self.sb.root}/usr/local/share/vaultcenter/0.1.0"

    def run_update(self, *extra, **kwargs):
        return self.sb.run(*self.ARGS, *extra, **kwargs)

    def state_path(self):
        return self.sb.root / "var/lib/vaultcenter-install/state"

    def updater_calls(self):
        return self.sb.calls("update.py")

    def test_update_runs_the_new_versions_updater(self):
        result = self.run_update(env={"DOCKER_CONFIG": "/elsewhere"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.updater_calls(),
                         [["--image", DIGEST, "--version", "0.1.0", "--payload", self.payload, "--yes"]])
        self.assertTrue((Path(self.payload) / "host/update.py").is_file())
        self.assertTrue((Path(self.payload) / "mhvtl-source.tar.gz").is_file())
        env = self.sb.call_envs("update.py")[0]
        self.assertEqual(env["VAULTCENTER_INSTALL_ROOT"], str(self.sb.root))
        self.assertEqual(env["DOCKER_CONFIG"], f"{self.sb.root}/etc/vaultcenter-install/docker")
        self.assertIn(["pull", "ghcr.io/adminsyspro/vaultcenter:latest"],
                      [c[:2] for c in self.sb.calls("docker")])

    def test_update_runs_only_its_own_steps(self):
        result = self.run_update("--quiet")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = [line for line in result.stdout.splitlines() if re.match(r"^\[\d+/7\] ", line)]
        self.assertEqual([re.match(r"^\[(\d+)/7\] (.+?): ", line).group(1, 2) for line in lines],
                         [(str(n + 1), label) for n, label in enumerate(UPDATE_LABELS)])
        for tool in ("dkms", "modprobe", "sysctl", "udevadm", "openssl"):
            self.assertEqual(self.sb.calls(tool), [], tool)
        self.assertEqual([c for c in self.sb.calls("docker") if "create-user" in c or "up" in c], [])
        self.assertEqual([c for c in self.sb.calls("systemctl") if c[0] != "is-active"], [])
        self.assertEqual(self.sb.calls("apt-get"), [])

    def test_update_results_map_to_exit_codes(self):
        for name, code in RESULT_EXIT.items():
            with self.subTest(name):
                self.sb.updater({"result": name, "reasons": ["a reason"]}, exit=code)
                result = self.run_update()
                self.assertEqual(result.returncode, code, result.stdout + result.stderr)

    def test_exit_code_and_result_that_disagree_take_the_more_severe(self):
        cases = [("updated", 3, 3), ("unchanged", 5, 5), ("refused", 0, 3), ("rolled_back", 6, 6),
                 ("rolled_back", 3, 5), ("updated", 1, 4)]
        for name, rc, expected in cases:
            with self.subTest(name=name, rc=rc):
                self.sb.updater({"result": name}, exit=rc)
                result = self.run_update()
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                self.assertIn("do not agree", result.stderr)
                self.assertIn("do not agree", self.sb.log())

    def test_no_json_from_the_updater_is_a_failure_showing_code_and_log(self):
        for answer in ("", "Traceback (most recent call last)\n", '{"result": "bizarre"}\n'):
            with self.subTest(answer=answer):
                self.sb.updater(answer, exit=7, stderr="progression\n")
                result = self.run_update()
                self.assertEqual(result.returncode, 4, result.stdout + result.stderr)
                self.assertIn("no readable result", result.stderr)
                self.assertIn("exit code 7", result.stderr)
                self.assertIn("/var/log/vaultcenter-install.log", result.stderr)

    def test_updater_progress_is_shown_and_logged_and_stdout_is_not(self):
        self.sb.updater({"result": "updated"}, stderr="Step 1: backup\nStep 2: container\n")
        result = self.run_update()
        self.assertIn("Step 1: backup\nStep 2: container", result.stderr)
        self.assertIn("update: Step 2: container", self.sb.log())
        self.assertNotIn('"result"', result.stdout)

    def test_refusal_reasons_are_printed(self):
        self.sb.updater({"result": "refused", "reasons": ["Version older than the installed one.", "Disk full."],
                         "backup": None}, exit=3)
        result = self.run_update()
        self.assertEqual(result.returncode, 3)
        for text in ("Update refused", "nothing has been changed",
                     "- Version older than the installed one.", "- Disk full."):
            self.assertIn(text, result.stdout)

    def test_yes_is_forwarded(self):
        self.run_update()
        self.assertIn("--yes", self.updater_calls()[0])

    def test_without_yes_the_first_call_has_none(self):
        self.sb.updater({"result": "refused", "reasons": ["Disk full."]}, exit=3)
        result = self.sb.run("--token", TOKEN, tty=True, tty_input="y\n")
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertEqual(len(self.updater_calls()), 1)
        self.assertNotIn("--yes", self.updater_calls()[0])
        self.assertNotIn("No backup job is using", result.stdout)  # not a guard refusal: no question

    def guard_scenario(self):
        self.sb.updater({"result": "refused", "reasons": [GUARD_REASON], "backup": None,
                         "confirmable": True}, exit=3)
        self.sb.updater({"result": "updated"}, contains=["--yes"])

    def test_guard_refusal_is_confirmed_on_the_terminal(self):
        self.guard_scenario()
        result = self.sb.run("--token", TOKEN, tty=True, tty_input="y\ny\n")
        self.assertEqual(result.returncode, 0, result.stdout)
        calls = self.updater_calls()
        self.assertEqual(len(calls), 2)
        self.assertNotIn("--yes", calls[0])
        self.assertIn("--yes", calls[1])
        self.assertIn(GUARD_REASON, result.stdout)
        self.assertIn("do you confirm?", result.stdout)

    def test_guard_refusal_declined_on_the_terminal_stops_with_the_reasons(self):
        self.guard_scenario()
        result = self.sb.run("--token", TOKEN, tty=True, tty_input="y\nn\n")
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertEqual(len(self.updater_calls()), 1)
        self.assertEqual(result.stdout.count(GUARD_REASON), 2)  # the question, then the final box

    def test_guard_refusal_with_yes_is_not_asked_again(self):
        self.guard_scenario()
        self.sb.updater({"result": "refused", "reasons": [GUARD_REASON], "confirmable": True}, exit=3,
                        contains=["--yes"])
        result = self.sb.run(*self.ARGS, tty=True)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(len(self.updater_calls()), 1)
        self.assertNotIn("do you confirm?", result.stdout)

    def test_a_refusal_that_is_not_confirmable_is_never_asked(self):
        # The text must not decide, the flag does.
        reason = "Not enough free space for the backup in /var/lib/vaultcenter-host/backups: 1 MiB."
        for fields in ({"confirmable": False}, {}, {"confirmable": "true"}):
            with self.subTest(fields):
                self.sb.reset_calls()
                answer = {"result": "refused", "reasons": [reason], "from": "0.0.9", "to": "0.1.0",
                          "warnings": [], "backup": None, "maintenance_left": False, "step": None, **fields}
                self.sb.updater(json.dumps(answer) + "\n", exit=3)  # raw: the key may be absent
                result = self.sb.run("--token", TOKEN, tty=True, tty_input="y\ny\n")
                self.assertEqual(result.returncode, 3, result.stdout)
                self.assertEqual(len(self.updater_calls()), 1)
                self.assertNotIn("do you confirm?", result.stdout)
                self.assertIn(reason, result.stdout)

    def test_a_confirmable_refusal_is_asked_whatever_its_wording(self):
        self.sb.updater({"result": "refused", "reasons": ["Any text at all."], "confirmable": True}, exit=3)
        self.sb.updater({"result": "updated"}, contains=["--yes"])
        result = self.sb.run("--token", TOKEN, tty=True, tty_input="y\ny\n")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(len(self.updater_calls()), 2)

    def test_the_summary_says_what_happened_for_each_result(self):
        base = {"from": "0.0.9", "to": "0.1.0", "backup": BACKUP}
        cases = {
            "updated": ({"warnings": ["A warning."], "maintenance_left": True},
                        ["VaultCenter is up to date", "0.0.9 -> 0.1.0", BACKUP, "A warning."], ["still in maintenance"]),
            "updated_left": ({"result": "updated", "maintenance_left": False,
                              "still_in_maintenance": [{"vtl": "1", "name": "VTL 1",
                                                        "reason": "it could not leave maintenance"}],
                              "reasons": ["VTL 1 could not leave maintenance (code 1)."]},
                             ["still in maintenance", "- VTL 1: it could not leave maintenance",
                              "- VTL 1 could not leave maintenance (code 1).", BACKUP], []),
            "unchanged": ({"from": "0.1.0", "warnings": ["A verified update has been finished."]},
                          ["already at version 0.1.0", "A verified update has been finished."], ["still in maintenance"]),
            "refused": ({"reasons": ["Reason A.", "Reason B."], "backup": None},
                        ["Update refused", "nothing has been changed", "- Reason A.", "- Reason B."], ["VaultCenter is up to date"]),
            "rolled_back": ({"reasons": ["Verification failed."], "maintenance_left": False,
                             "still_in_maintenance": [{"vtl": "1", "name": "VTL 1", "reason": "rollback"}]},
                            ["STILL IN MAINTENANCE", "decision", "console", BACKUP, "- Verification failed.",
                             "previous version", "- VTL 1: rollback"], ["VaultCenter is up to date"]),
            "rollback_failed": ({"step": "restoring the database", "reasons": ["Impossible."]},
                                ["restoring the database", BACKUP, LOCAL_DOC, "Nothing is retried", "- Impossible."],
                                ["VaultCenter is up to date"]),
        }
        for key, (fields, wanted, unwanted) in cases.items():
            name = fields.get("result", key)
            with self.subTest(key):
                self.sb.updater({**base, **fields, "result": name}, exit=RESULT_EXIT[name])
                result = self.run_update()
                for text in wanted:
                    self.assertIn(text, result.stdout)
                for text in unwanted:
                    self.assertNotIn(text, result.stdout)
                self.assertIn("/var/log/vaultcenter-install.log", result.stdout)

    def test_rolled_back_box_says_which_vtls_are_still_in_maintenance(self):
        still = [{"vtl": "1", "name": "VTL 1", "reason": "the rollback leaves it in maintenance"},
                 {"vtl": None, "name": "the library", "reason": "the rollback leaves it in maintenance"}]
        self.sb.updater({"result": "rolled_back", "backup": BACKUP, "reasons": ["x"], "maintenance_left": False,
                         "still_in_maintenance": still}, exit=5)
        result = self.run_update()
        self.assertEqual(result.returncode, 5)
        self.assertIn("THESE VTLS ARE STILL IN MAINTENANCE", result.stdout)
        self.assertIn("- VTL 1: the rollback leaves it in maintenance", result.stdout)
        self.assertIn("- the library: the rollback leaves it in maintenance", result.stdout)

    def test_rolled_back_box_says_nothing_of_maintenance_when_no_vtl_was_entered(self):
        self.sb.updater({"result": "rolled_back", "backup": BACKUP, "reasons": ["x"]}, exit=5)
        result = self.run_update()
        self.assertEqual(result.returncode, 5)
        self.assertNotIn("EN MAINTENANCE", result.stdout)

    def test_update_that_entered_no_vtl_does_not_announce_any_in_maintenance(self):
        # The first update of an appliance without an active VTL: the updater
        # entered none, so none is still in maintenance, whatever maintenance_left says.
        for left in (True, False):
            with self.subTest(maintenance_left=left):
                self.sb.updater({"result": "updated", "maintenance_left": left, "still_in_maintenance": []})
                result = self.run_update()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("VaultCenter is up to date", result.stdout)
                self.assertNotIn("maintenance", result.stdout)
                self.assertNotIn("Reasons", result.stdout)

    def test_reasons_and_warnings_are_printed_for_every_result(self):
        reason = "The old version vc-0123456789ab of the module could not be removed from DKMS."
        warning = "A warning."
        for name, code in RESULT_EXIT.items():
            with self.subTest(name):
                self.sb.updater({"result": name, "reasons": [reason], "warnings": [warning]}, exit=code)
                result = self.run_update()
                self.assertEqual(result.returncode, code, result.stdout + result.stderr)
                self.assertIn("- " + reason, result.stdout)
                self.assertIn("- " + warning, result.stdout)

    def step_line(self, result):
        return next(line for line in result.stdout.splitlines() if "Appliance update" in line)

    def test_step_line_reflects_the_result(self):
        said = {"updated": "to 0.1.0 finished", "unchanged": "already at version", "refused": "update refused",
                "rolled_back": "update cancelled", "rollback_failed": "rollback failed"}
        for name, code in RESULT_EXIT.items():
            with self.subTest(name):
                self.sb.updater({"result": name, "reasons": ["a reason"]}, exit=code)
                plain = self.run_update()
                self.assertIn(said[name], self.step_line(plain))
                self.assertIn("unsuccessful" if code else "ok:", self.sb.log().split("Appliance update")[-1])
                fancy = self.run_update(tty=True, env={"NO_COLOR": "1"})
                marks = [line.strip() for line in fancy.stdout.splitlines() if said[name] in line and line.strip()[:1] in "✔✘"]
                self.assertEqual([mark[0] for mark in marks], ["✘" if code else "✔"], fancy.stdout)

    def test_signal_during_the_update_waits_for_the_result_and_shows_it(self):
        still = [{"vtl": "1", "name": "VTL 1", "reason": "the rollback leaves it in maintenance"}]
        for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            name = number.name[3:]
            with self.subTest(name):
                sb = Sandbox()
                self.addCleanup(sb.close)
                sb.provide_docker()
                compose = sb.root / self.HOST / "compose.json"
                compose.parent.mkdir(parents=True)
                compose.write_text("{}")
                # The updater, interrupted like the installer, rolls back and reports.
                sb.updater({"result": "rolled_back", "backup": BACKUP, "maintenance_left": False,
                            "still_in_maintenance": still,
                            "reasons": ["Step \"container\" failed: signal received"]},
                           exit=5, stderr="rollback: stopping the container\n", block="signal")
                process = sb.start(*self.ARGS)
                self.addCleanup(process.wait)
                self.assertTrue(sb.wait_blocked(), "the updater never started")
                os.killpg(process.pid, number)
                stdout, stderr = process.communicate(timeout=30)
                self.assertEqual(process.returncode, 5, stdout + stderr)
                for text in ("Update cancelled", BACKUP, "- Step \"container\" failed: signal received",
                             "- VTL 1: the rollback leaves it in maintenance"):
                    self.assertIn(text, stdout)
                self.assertIn("rollback: stopping the container", stderr)
                self.assertIn(f"Signal {name} received during the update", stderr)
                self.assertNotIn("do not agree", stderr)
                log = sb.log()
                self.assertIn('update result: {"result": "rolled_back"', log)
                self.assertIn("update: rollback: stopping the container", log)
                self.assertIn(f"signal {name} received during the update", log)
                self.assertEqual(list((sb.root / "var/lib/vaultcenter-install").glob("updater-output.*")), [])

    def test_signal_during_an_update_that_succeeds_exits_with_its_code(self):
        self.sb.updater({"result": "updated"}, block="signal")
        process = self.sb.start(*self.ARGS)
        self.addCleanup(process.wait)
        self.assertTrue(self.sb.wait_blocked(), "the updater never started")
        os.killpg(process.pid, signal.SIGINT)
        stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, stdout + stderr)
        self.assertIn("VaultCenter is up to date", stdout)
        self.assertIn("Signal INT received", stderr)

    def test_signal_outside_the_updater_still_stops_the_installer(self):
        self.sb.scenario("docker", ["cp"], block=True)
        process = self.sb.start(*self.ARGS)
        self.addCleanup(process.wait)
        self.assertTrue(self.sb.wait_blocked(), "docker cp never started")
        os.killpg(process.pid, signal.SIGHUP)
        process.communicate(timeout=20)
        self.assertEqual(process.returncode, 129)
        self.assertIn(["rm", "-f", "vaultcenter-install-payload"], self.sb.calls("docker"))
        self.assertEqual(self.updater_calls(), [])

    def test_update_never_recreates_the_administrator_or_the_certificate(self):
        cold = self.sb.root / self.HOST / "config/cold-vtl"
        (cold / "tls").mkdir(parents=True)
        files = {cold / "auth.env": b"COLD_VTL_PUBLIC_URL=https://old:8443\n",
                 cold / "tls/fullchain.pem": b"cert\n", cold / "tls/privkey.pem": b"key\n"}
        for path, data in files.items():
            path.write_bytes(data)
            os.utime(path, (1_000_000_000, 1_000_000_000))
        before = {path: (path.read_bytes(), os.stat(path).st_mtime_ns) for path in files}
        result = self.run_update()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual({path: (path.read_bytes(), os.stat(path).st_mtime_ns) for path in files}, before)
        self.assertEqual([c for c in self.sb.calls("docker") if "create-user" in c], [])
        self.assertEqual([c for c in self.sb.calls("openssl") if c[:1] == ["req"]], [])
        self.assertEqual(self.sb.stdin("docker"), [REGISTRY_PASSWORD + ""])  # the registry login only
        self.assertNotRegex(result.stdout, r"Password")

    def test_update_writes_nothing_to_the_state_file(self):
        self.assertEqual(self.run_update().returncode, 0)
        self.assertFalse(self.state_path().exists())

    def test_a_leftover_state_file_is_ignored_and_left_as_it_was(self):
        stale = "image=ghcr.io/adminsyspro/vaultcenter@sha256:" + "f" * 64 + "\nversion=9.9.9\ndone=image\ndone=payload\n"
        self.state_path().parent.mkdir(parents=True)
        self.state_path().write_text(stale)
        self.assertEqual(self.run_update().returncode, 0)
        self.assertEqual(self.state_path().read_text(), stale)
        self.assertEqual(self.updater_calls()[0][:4], ["--image", DIGEST, "--version", "0.1.0"])
        self.assertTrue([c for c in self.sb.calls("docker") if c[:1] == ["pull"]])

    def test_a_failed_update_leaves_no_state_either(self):
        self.sb.scenario("docker", ["pull"], exit=1)
        self.assertEqual(self.run_update().returncode, 4)
        self.assertFalse(self.state_path().exists())

    def test_missing_docker_in_update_mode_is_a_failure_never_an_installation(self):
        sb = Sandbox()
        self.addCleanup(sb.close)
        (sb.root / self.HOST).mkdir(parents=True)
        (sb.root / self.HOST / "compose.json").write_text("{}")
        result = sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 4, result.stdout + result.stderr)
        self.assertIn("Docker", result.stderr)
        self.assertEqual([c for c in sb.calls("apt-get")], [])
        self.assertFalse((sb.root / "etc/apt/sources.list.d/docker.list").exists())
        self.assertEqual(sb.calls("update.py"), [])

    def test_docker_without_compose_in_update_mode_is_a_failure(self):
        self.sb.scenario("docker", ["compose", "version"], exit=1)
        result = self.run_update()
        self.assertEqual(result.returncode, 4)
        self.assertEqual(self.sb.calls("apt-get"), [])

    def test_a_changed_proxy_is_never_applied_by_restarting_the_daemon(self):
        dropin = self.sb.root / "etc/systemd/system/docker.service.d/vaultcenter-proxy.conf"
        dropin.parent.mkdir(parents=True)
        dropin.write_text("[Service]\nEnvironment=HTTPS_PROXY=http://old:1\n")
        result = self.run_update(env={"HTTPS_PROXY": "http://new:2"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(dropin.read_text(), "[Service]\nEnvironment=HTTPS_PROXY=http://old:1\n")
        self.assertEqual([c for c in self.sb.calls("systemctl") if c[0] != "is-active"], [])
        self.assertIn("/etc/systemd/system/docker.service.d/vaultcenter-proxy.conf", result.stderr)
        self.assertIn("systemctl daemon-reload && systemctl restart docker", result.stderr)

    def test_a_new_proxy_does_not_create_the_dropin_either(self):
        result = self.run_update(env={"HTTPS_PROXY": "http://new:2"})
        self.assertEqual(result.returncode, 0)
        self.assertFalse((self.sb.root / "etc/systemd/system/docker.service.d").exists())
        self.assertEqual([c for c in self.sb.calls("systemctl") if c[0] != "is-active"], [])
        self.assertIn("systemctl restart docker", result.stderr)

    def test_an_unchanged_proxy_dropin_says_nothing(self):
        self.run_update(env={"HTTPS_PROXY": "http://p:1"})
        dropin = self.sb.root / "etc/systemd/system/docker.service.d/vaultcenter-proxy.conf"
        dropin.parent.mkdir(parents=True)
        dropin.write_text("[Service]\nEnvironment=HTTPS_PROXY=http://p:1\n")
        result = self.run_update(env={"HTTPS_PROXY": "http://p:1"})
        self.assertNotIn("Warning", result.stderr)

    def test_resume_rollback_options_are_passed_to_the_updater(self):
        self.assertEqual(self.run_update("--resume-rollback").returncode, 0)
        self.assertEqual(self.updater_calls()[-1][-2:], ["--yes", "--resume-rollback"])
        self.assertEqual(self.run_update("--resume-rollback", "--force-rollback").returncode, 0)
        self.assertEqual(self.updater_calls()[-1][-3:], ["--yes", "--resume-rollback", "--force-rollback"])
        self.assertEqual(self.updater_calls()[-1][:6], ["--image", DIGEST, "--version", "0.1.0", "--payload", self.payload])
        self.run_update("--version", "0.1.0")
        self.assertNotIn("--resume-rollback", self.updater_calls()[-1])

    def test_force_rollback_alone_is_a_usage_error(self):
        result = self.run_update("--force-rollback")
        self.assertEqual(result.returncode, 2)
        self.assertIn("--resume-rollback", result.stderr)
        self.assertEqual(self.sb.calls("docker"), [])

    def test_rollback_options_are_refused_outside_update_mode(self):
        for option in ("--resume-rollback", "--force-rollback"):
            with self.subTest(option):
                sb = Sandbox()
                self.addCleanup(sb.close)
                sb.provide_docker()
                args = ("--resume-rollback", "--force-rollback") if option == "--force-rollback" else (option,)
                result = sb.run(*self.ARGS, *args)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("update mode", result.stderr)
                self.assertEqual([c for c in sb.calls() if c[0] not in ("id", "docker")], [])
                self.assertEqual([c for c in sb.calls("docker") if c[0] != "ps"], [])

    def test_usage_text_mentions_the_rollback_options(self):
        result = self.run_update("--force-rollback")
        self.assertIn("same --version", result.stderr)


OLD_DIGEST = "ghcr.io/adminsyspro/vaultcenter@sha256:" + "a" * 64
STATE_VOLUME = "var/lib/docker/volumes/vaultcenter_state/_data"


class RealUpdaterTest(unittest.TestCase):
    """install.sh with the real update.py of the payload, not its stand-in: the
    arguments, the environment, the JSON and the exit code as they really are."""
    ARGS = ("--token", TOKEN, "--yes")

    def setUp(self):
        self.sb = sb = Sandbox()
        self.addCleanup(sb.close)
        sb.provide_docker()
        sb.real_updater()
        self.root = sb.root
        self.compose = self.root / "etc/vaultcenter-host/compose.json"
        self.compose.parent.mkdir(parents=True)
        (self.root / "etc/vaultcenter-host/storage.json").write_text(json.dumps({
            "compose_file": "/etc/vaultcenter-host/compose.json", "project": "vaultcenter",
            "container": "vaultcenter-appliance-1", "state_dir": "/" + STATE_VOLUME, "isolated": True}))
        state = self.root / STATE_VOLUME
        state.mkdir(parents=True)
        with closing(sqlite3.connect(state / "catalog.db")) as db:
            db.executescript("""
                CREATE TABLE jobs (id TEXT PRIMARY KEY, kind TEXT, subject TEXT, status TEXT);
                CREATE TABLE vtls (id TEXT PRIMARY KEY, lifecycle TEXT);
                CREATE TABLE vtl_provisioning (vtl_id TEXT PRIMARY KEY, phase TEXT);
                INSERT INTO jobs VALUES ('j1', 'export', 'A00001', 'succeeded');
            """)
            db.commit()
        install = self.root / "var/lib/vaultcenter-install"
        install.mkdir(parents=True)
        (install / "mhvtl.sha256").write_text(sb.mhvtl_archive_sha256 + "\n")
        self.libexec = self.root / "usr/local/libexec/vaultcenter-host"
        self.libexec.mkdir(parents=True)
        (self.libexec / "storage_controller.py").write_text("# old controller\n")
        (self.root / "etc/systemd/system").mkdir(parents=True)

    def installed(self, image):
        self.compose.write_text(json.dumps({"name": "vaultcenter", "services": {"appliance": {"image": image}}}))

    def image(self):
        return json.loads(self.compose.read_text())["services"]["appliance"]["image"]

    def backups(self):
        directory = self.root / "var/lib/vaultcenter-host/backups"
        return sorted(path.name for path in directory.iterdir()) if directory.is_dir() else []

    def recreations(self):
        return [c for c in self.sb.calls("docker") if c[:1] == ["compose"] and "--force-recreate" in c]

    def assert_real_updater_ran(self):
        self.assertEqual(self.sb.calls("update.py"), [])       # the stand-in was not used
        self.assertTrue((self.root / "var/lib/vaultcenter-install/update.lock").is_file())
        self.assertIn("update result: {\"result\": ", self.sb.log())

    def test_same_digest_is_unchanged(self):
        self.installed(DIGEST)
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_real_updater_ran()
        self.assertIn("already at version 0.1.0", result.stdout)
        self.assertIn("[update] the target image is already installed", result.stderr)
        self.assertNotIn("maintenance", result.stdout)
        self.assertEqual(self.backups(), [])
        self.assertEqual(self.recreations(), [])

    def test_blocker_is_refused_with_its_reason(self):
        self.installed(OLD_DIGEST)
        with closing(sqlite3.connect(self.root / STATE_VOLUME / "catalog.db")) as db:
            db.execute("INSERT INTO jobs VALUES ('j2', 'export', 'A00002', 'queued')")
            db.commit()
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assert_real_updater_ran()
        self.assertIn("Update refused", result.stdout)
        self.assertIn("- Unfinished jobs in the catalogue (1 queued)", result.stdout)
        self.assertIn("Appliance update: update refused", result.stdout)
        self.assertEqual(self.image(), OLD_DIGEST)
        self.assertEqual(self.backups(), [])
        self.assertEqual(self.recreations(), [])
        self.assertEqual((self.libexec / "storage_controller.py").read_text(), "# old controller\n")

    def test_update_goes_through_and_is_summarised(self):
        self.installed(OLD_DIGEST)
        result = self.sb.run(*self.ARGS, env={"HTTPS_PROXY": "http://proxy.example:3128"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_real_updater_ran()
        self.assertIn("VaultCenter is up to date", result.stdout)
        self.assertIn("0.1.0 -> 0.1.0", result.stdout)
        # No VTL was entered: none is announced as still in maintenance.
        self.assertNotIn("en maintenance", result.stdout)
        self.assertEqual(self.image(), DIGEST)
        self.assertEqual(len(self.backups()), 1)
        self.assertIn(f"/var/lib/vaultcenter-host/backups/{self.backups()[0]}", result.stdout)
        self.assertEqual(len(self.recreations()), 1)
        shipped = self.root / "usr/local/share/vaultcenter/0.1.0/host/storage_controller.py"
        self.assertEqual((self.libexec / "storage_controller.py").read_text(), shipped.read_text())
        self.assertTrue((self.root / "etc/systemd/system/vaultcenter-mhvtl.service").is_file())
        self.assertIn(["enable", "vaultcenter-mhvtl.service"], self.sb.calls("systemctl"))
        for name in ("update-in-progress.json", "update-verified.json"):
            self.assertFalse((self.root / "var/lib/vaultcenter-host" / name).exists(), name)
        # The updater read /health on the loopback, not through the proxy.
        probes = [c for c in self.sb.calls("curl") if "https://127.0.0.1:8443/health" in c]
        self.assertTrue(probes)
        for probe in probes:
            self.assertIn("--noproxy", probe)
        # The module did not change: nothing was rebuilt, the appliance was not stopped for it.
        self.assertEqual([c for c in self.sb.calls("dkms") if c[0] != "status"], [])


class IsolationReceiptTest(unittest.TestCase):
    """The configuration step installs the receipt the payload ships, when it fits."""
    ARGS = ("--token", TOKEN, "--yes")
    RUNTIME = "# the isolation runtime\n"
    SENTENCE = "provides no isolation receipt"

    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.close)
        self.sb.provide_docker()
        self.installed = self.sb.root / "var/lib/docker/volumes/vaultcenter_state/_data/isolation-qualified.json"

    def ship(self, runtime_sha256=None, verified=True, runtime=True):
        if runtime:
            self.sb.payload_file("vtl_runtime.py", self.RUNTIME)
        digest = runtime_sha256 or hashlib.sha256(self.RUNTIME.encode()).hexdigest()
        self.sb.payload_file("isolation-qualified.json",
                             json.dumps({"runtime_sha256": digest, "isolation_verified": verified}))

    def test_a_matching_receipt_is_installed_private(self):
        self.ship()
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.installed.is_file())
        self.assertEqual(stat.S_IMODE(os.stat(self.installed).st_mode), 0o600)
        self.assertEqual(json.loads(self.installed.read_text())["isolation_verified"], True)
        self.assertNotIn("isolation receipt", result.stdout)

    def test_a_receipt_for_another_runtime_is_not_installed(self):
        for case in ("digest", "unverified", "no_runtime"):
            with self.subTest(case):
                sb = Sandbox()
                self.addCleanup(sb.close)
                sb.provide_docker()
                self.sb = sb
                if case == "digest":
                    self.ship("0" * 64)
                elif case == "unverified":
                    self.ship(verified=False)
                else:
                    self.ship(runtime=False)
                installed = sb.root / "var/lib/docker/volumes/vaultcenter_state/_data/isolation-qualified.json"
                result = sb.run(*self.ARGS)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(installed.exists())
                self.assertIn(self.SENTENCE, result.stdout)
                self.assertIn("S3", result.stdout)
                self.assertIn(LOCAL_DOC, result.stdout)

    def test_a_payload_without_receipt_installs_nothing_and_says_so(self):
        result = self.sb.run(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.installed.exists())
        self.assertIn(self.SENTENCE, result.stdout)
        self.assertIn("refused", result.stdout)


FAILURES = {
    "preflight": (("nproc", []), {"stdout": "1\n"}),
    "credentials": (("curl", []), {"contains": [VALIDATE_URL], "exit": 7}),
    "docker": (("apt-get", []), {"contains": ["install", "docker-ce"], "exit": 100}),
    "image": (("docker", ["pull"]), {"exit": 1}),
    "payload": (("docker", ["cp"]), {"exit": 1}),
    "kernel_module": (("dkms", ["build"]), {"exit": 10}),
    "host_service": (("docker", ["compose", "-f"]), {"contains": ["config", "json"], "exit": 1}),
    "configuration": (("openssl", ["req"]), {"exit": 1}),
    "administrator": (("docker", ["compose", "-f"]), {"contains": ["create-user"], "exit": 1}),
    "start": (("docker", ["inspect", "--format", HEALTH_FORMAT]), {"stdout": "unhealthy\n"}),
    "summary": (("openssl", ["x509"]), {"exit": 1}),
}
# What each finished step runs, to check that a resumed installation does not run it again.
RAN = {
    "preflight": lambda c: False,
    "credentials": lambda c: False,
    "docker": lambda c: c[0] == "apt-get" and "docker-ce" in c,
    "image": lambda c: c[:2] == ["docker", "pull"],
    "payload": lambda c: c[:2] == ["docker", "create"],
    "kernel_module": lambda c: c[0] in ("dkms", "modprobe"),
    "host_service": lambda c: c[0] == "docker" and "config" in c and "json" in c,
    "configuration": lambda c: c[:2] == ["openssl", "req"],
    "administrator": lambda c: "create-user" in c,
    "start": lambda c: c[0] == "docker" and "up" in c,
    "summary": lambda c: False,
}


if __name__ == "__main__":
    unittest.main()
