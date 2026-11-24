from pathlib import Path
import hashlib
import io
import os
import re
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).parents[2]
HOST_DIR = '/usr/local/share/vaultcenter/host/'
SHIPPED = [
    'install.sh', 'prepare-dkms-source.sh', 'dkms.conf', 'install.py', 'storage_controller.py',
    'storage_paths.py', 'vtl_state.py', 'vtl_storage.py', 'storage_mounts.py',
    'vaultcenter-storage-host.service', 'vaultcenter-storage-mounts.service',
    'vaultcenter-mhvtl.service', 'vaultcenter-mhvtl.conf', '99-vaultcenter-console.conf',
    '99-vaultcenter.rules', 'compose.yaml', 'auth.env.example', 'update.py', 'vtl_runtime.py',
    'install.md',
]
# The isolation receipt of the shipped vtl_runtime.py; the Dockerfile copies it when present.
OPTIONAL_RECEIPT = 'deploy/cold-vtl/isolation-qualified.jso[n]'


def host_copy_sources():
    """Source tokens of the Dockerfile COPY lines that fill the host payload."""
    lines = [line for line in (ROOT / 'Dockerfile').read_text().splitlines()
             if line.startswith('COPY ') and HOST_DIR in line]
    return [token for line in lines for token in line.split()[1:-1] if not token.startswith('--')]


class InstallPayloadTest(unittest.TestCase):
    def test_dockerfile_ships_every_host_file(self):
        lines = [line for line in (ROOT / 'Dockerfile').read_text().splitlines()
                 if line.startswith('COPY ') and HOST_DIR in line]
        copied = {Path(token).name for line in lines
                  for token in line.split()[1:-1] if not token.startswith('--')}
        for name in SHIPPED:
            self.assertIn(name, copied)

    def test_dockerignore_lets_the_host_files_in(self):
        allowed = (ROOT / '.dockerignore').read_text().splitlines()
        sources = [re.sub(r'\[(.)\]', r'\1', token) for token in host_copy_sources()]
        self.assertIn('deploy/host/update.py', sources)
        for path in sources:
            self.assertTrue(any(line.startswith('!') and line[1:] in (path, '/'.join(path.split('/')[:-1]) + '/*')
                                for line in allowed), path)

    def test_only_the_installation_guide_of_docs_enters_the_build_context(self):
        lines = (ROOT / '.dockerignore').read_text().splitlines()
        self.assertIn('docs/install.md', host_copy_sources())
        # The directory is let in, its content shut out, then the one file let in:
        # the last matching line wins.
        order = [lines.index(line) for line in ('!docs/', 'docs/*', '!docs/install.md')]
        self.assertEqual(order, sorted(order))
        self.assertEqual([line for line in lines if line.startswith(('!docs', 'docs'))],
                         ['!docs/', 'docs/*', '!docs/install.md'])

    def test_isolation_receipt_is_optional_in_the_payload(self):
        # A bracket glob that matches nothing is accepted by COPY as long as
        # another source of the same line exists (vtl_runtime.py).
        lines = [line for line in (ROOT / 'Dockerfile').read_text().splitlines()
                 if line.startswith('COPY ') and OPTIONAL_RECEIPT in line.split()]
        self.assertEqual(len(lines), 1)
        self.assertIn('deploy/cold-vtl/vtl_runtime.py', lines[0].split())
        self.assertTrue(lines[0].endswith(HOST_DIR))

    def test_shipped_receipt_matches_the_shipped_runtime(self):
        # Without a matching receipt no VTL can be activated on a new installation,
        # and install.sh only logs that it skipped the receipt. A change to
        # vtl_runtime.py needs a new isolation acceptance and a new receipt.
        import json
        receipt = json.loads((ROOT / 'deploy/cold-vtl/isolation-qualified.json').read_text())
        runtime = hashlib.sha256((ROOT / 'deploy/cold-vtl/vtl_runtime.py').read_bytes()).hexdigest()
        self.assertIs(receipt['isolation_verified'], True)
        self.assertEqual(receipt['runtime_sha256'], runtime,
                         'vtl_runtime.py changed: run the isolation acceptance and ship its new receipt')

    def test_dkms_conf_declares_the_module(self):
        text = (ROOT / 'deploy/install/dkms.conf').read_text()
        for line in ['PACKAGE_NAME="mhvtl"', 'PACKAGE_VERSION="@VERSION@"',
                     'BUILT_MODULE_NAME[0]="mhvtl"',
                     'DEST_MODULE_LOCATION[0]="/updates/dkms"', 'AUTOINSTALL="yes"']:
            self.assertIn(line, text.splitlines())
        self.assertEqual(text.count('@VERSION@'), 1)
        self.assertRegex(text, r'(?m)^MAKE\[0\]=.*KDIR=/lib/modules/\$\{kernelver\}/build')

    def test_smoke_compares_the_shipped_installer(self):
        text = (ROOT / 'deploy/docker/smoke.sh').read_text()
        self.assertRegex(text, re.escape(HOST_DIR + 'install.sh') + r'[^\n]*sha256sum')
        self.assertIn('deploy/install/install.sh', text)

    def test_installer_skeleton_is_executable(self):
        path = ROOT / 'deploy/install/install.sh'
        self.assertTrue(path.stat().st_mode & 0o100)
        self.assertEqual(path.read_text().splitlines()[0], '#!/usr/bin/env bash')

    def test_ci_dkms_job_applies_the_dockerfile_patches_in_its_order(self):
        dockerfile = [m for m in re.findall(r'git apply /tmp/([\w-]+)\.patch',
                                            (ROOT / 'Dockerfile').read_text())]
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        loop = re.search(r'for patch in ([\w -]+); do', ci)
        self.assertEqual(loop.group(1).split(), dockerfile)
        self.assertIn('prepare-dkms-source.sh', ci)

    def test_dockerfile_and_ci_archive_the_module_sources_with_the_same_script(self):
        # The sha256 of the archive is the module version: one script makes it,
        # and neither file archives or commits the tree by its own commands.
        dockerfile = (ROOT / 'Dockerfile').read_text()
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('COPY deploy/mhvtl/source-archive.sh /tmp/source-archive.sh', dockerfile)
        self.assertRegex(dockerfile, r'sh /tmp/source-archive\.sh /src/mhvtl /stage/usr/share/doc/mhvtl/source\.tar\.gz')
        self.assertRegex(ci, r'sh deploy/mhvtl/source-archive\.sh "\$src" /tmp/mhvtl-source\.tar\.gz')
        for name, text in (('Dockerfile', dockerfile), ('ci.yml', ci)):
            for forbidden in ('git archive', ' archive --format', 'commit -m', 'git add usr'):
                self.assertNotIn(forbidden, text, name)
        revision = re.search(r'ARG MHVTL_REVISION=([0-9a-f]{40})', dockerfile).group(1)
        self.assertIn(f'fetch --depth=1 origin {revision}', ci)
        self.assertIn('!deploy/mhvtl/source-archive.sh', (ROOT / '.dockerignore').read_text().splitlines())

    def test_dockerfile_archives_the_sources_before_building_them(self):
        lines = (ROOT / 'Dockerfile').read_text().splitlines()
        archive = next(i for i, line in enumerate(lines) if 'source-archive.sh /src/mhvtl' in line)
        build = next(i for i, line in enumerate(lines) if line.startswith('RUN make -j2 usr etc'))
        patches = next(i for i, line in enumerate(lines) if line.startswith('RUN git apply'))
        self.assertLess(patches, archive)
        self.assertLess(archive, build)

    def patched_tree(self, tmp, name):
        """A git tree like the mhvtl stage's: one upstream commit, then changes
        that are not committed (the patches)."""
        tree = tmp / name
        env = {**os.environ, 'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_NOSYSTEM': '1',
               'GIT_AUTHOR_NAME': 'Upstream', 'GIT_AUTHOR_EMAIL': 'up@example.test',
               'GIT_COMMITTER_NAME': 'Upstream', 'GIT_COMMITTER_EMAIL': 'up@example.test',
               'GIT_AUTHOR_DATE': '2020-02-02T02:02:02+0000', 'GIT_COMMITTER_DATE': '2020-02-02T02:02:02+0000'}
        for directory in ('usr', 'include', 'kernel'):
            (tree / directory).mkdir(parents=True)
            (tree / directory / 'file.c').write_text(f'/* upstream {directory} */\n')
        git = ['git', '-C', str(tree)]
        subprocess.run(['git', 'init', '--quiet', str(tree)], env=env, check=True)
        subprocess.run(git + ['add', '.'], env=env, check=True)
        subprocess.run(git + ['commit', '--quiet', '-m', 'upstream'], env=env, check=True)
        subprocess.run(git + ['checkout', '--quiet', '--detach'], env=env, check=True)
        (tree / 'kernel/file.c').write_text('/* patched */\n')
        (tree / 'kernel/new.c').write_text('/* added by a patch */\n')
        (tree / 'kernel/build.sh').write_text('#!/bin/sh\n')
        (tree / 'kernel/build.sh').chmod(0o755)
        return tree

    def test_source_archive_is_the_same_for_the_same_sources(self):
        script = ROOT / 'deploy/mhvtl/source-archive.sh'
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            digests = []
            # Whoever builds, whenever, under whatever git identity or umask.
            builds = (({'GIT_AUTHOR_NAME': 'Alice', 'GIT_COMMITTER_EMAIL': 'alice@example.test',
                        'GIT_AUTHOR_DATE': '2031-03-03T03:03:03+0000'}, 0o022),
                      ({'GIT_COMMITTER_NAME': 'Bob', 'GIT_COMMITTER_DATE': '2035-05-05T05:05:05+0000'}, 0o077))
            for index, (identity, umask) in enumerate(builds):
                tree = self.patched_tree(tmp, f'tree{index}')
                archive = tmp / f'source{index}.tar.gz'
                env = {**os.environ, 'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_NOSYSTEM': '1', **identity}
                subprocess.run(['sh', '-c', f'umask {umask:03o} && exec sh "$0" "$1" "$2"', str(script),
                                str(tree), str(archive)], env=env, check=True, capture_output=True)
                digests.append(hashlib.sha256(archive.read_bytes()).hexdigest())
                log = subprocess.run(['git', '-C', str(tree), 'log', '-1', '--format=%an <%ae> %at | %cn <%ce> %ct'],
                                     capture_output=True, text=True, check=True).stdout.strip()
                self.assertEqual(log, 'VaultCenter <build@localhost> 1767225600 | '
                                      'VaultCenter <build@localhost> 1767225600')
                with tarfile.open(archive) as tar:
                    members = {member.name: member for member in tar.getmembers()}
                self.assertEqual(sorted(name for name, member in members.items() if member.isfile()),
                                 ['include/file.c', 'kernel/build.sh', 'kernel/file.c', 'kernel/new.c',
                                  'usr/file.c'])
                self.assertEqual({member.mtime for member in members.values()}, {1767225600})
                self.assertEqual(members['kernel/file.c'].mode, 0o644)
                self.assertEqual(members['kernel/build.sh'].mode, 0o755)
                self.assertFalse((tmp / f'source{index}.tar.gz.tar').exists())
            self.assertEqual(digests[0], digests[1])

    def test_boot_guard_unit_rebuilds_the_module_before_docker(self):
        text = (ROOT / 'deploy/host/vaultcenter-mhvtl.service').read_text()
        lines = text.splitlines()
        self.assertIn('Before=docker.service', lines)
        after = next(line for line in lines if line.startswith('After='))
        self.assertIn('systemd-modules-load.service', after)
        self.assertIn('Type=oneshot', lines)
        self.assertIn('WantedBy=multi-user.target', lines)
        # A bounded wait, and nothing that makes the boot depend on its success.
        self.assertRegex(text, r'(?m)^TimeoutStartSec=\d+$')
        self.assertNotRegex(text, r'(?m)^(Requires|RequiredBy|BindsTo|OnFailure)=')
        command = next(line for line in lines if line.startswith('ExecStart='))
        self.assertRegex(command, r'modprobe mhvtl opts=0 .*dkms autoinstall -k %v && modprobe mhvtl opts=0')
        self.assertIn('echo', command)
        self.assertNotIn('$', command)   # systemd would expand it
        self.assertTrue(any('modprobe sg' in line for line in lines))

    def test_storage_service_counts_as_started_once_it_runs(self):
        # Type=exec: Before=docker.service then means the runtime directory exists.
        lines = (ROOT / 'deploy/host/vaultcenter-storage-host.service').read_text().splitlines()
        self.assertIn('Type=exec', lines)
        self.assertIn('Before=docker.service', lines)

    @staticmethod
    def ci_job(name):
        """The text of one job of the CI workflow."""
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        return re.search(rf'(?ms)^  {name}:\n(.*?)(?=^  [a-z]+:\n|\Z)', ci).group(1)

    def test_ci_dkms_job_runs_its_steps_with_bash(self):
        # A container job runs `sh -e` by default, and dash rejects `set -o pipefail`.
        job = self.ci_job('dkms')
        self.assertIn('container:', job)
        self.assertRegex(job, r'(?m)^    defaults:\n      run:\n        shell: bash$')
        self.assertIn('set -euo pipefail', job)

    def test_ci_publishes_the_image_only_when_the_module_compiles(self):
        needs = re.search(r'(?m)^    needs: \[(.*)\]$', self.ci_job('image')).group(1)
        self.assertEqual({name.strip() for name in needs.split(',')}, {'backend', 'ui', 'dkms'})

    def test_installer_tells_shellcheck_why_its_steps_look_unreachable(self):
        # The directive covers the file only on the line that follows the shebang.
        lines = (ROOT / 'deploy/install/install.sh').read_text().splitlines()
        self.assertEqual(lines[1], '# shellcheck disable=SC2317')
        self.assertTrue(lines[2].startswith('# (SC2317'))
        directives = [line for line in lines if 'shellcheck disable' in line]
        self.assertEqual(directives, ['# shellcheck disable=SC2317'])
        # What makes the directive necessary is still there.
        self.assertIn('"step_${name}"', '\n'.join(lines))

    def test_ci_shellchecks_the_installer_scripts(self):
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        lines = [line for line in ci.splitlines() if 'run: shellcheck' in line]
        self.assertTrue(any('deploy/install/*.sh' in line for line in lines), lines)

    def test_prepare_script_unpacks_and_versions_the_tree_once(self):
        script = ROOT / 'deploy/install/prepare-dkms-source.sh'
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            archive = tmp / 'source.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                data = b'all:\n'
                info = tarfile.TarInfo('kernel/Makefile')
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            args = ['bash', str(script), str(archive), str(tmp / 'root'),
                    str(ROOT / 'deploy/install/dkms.conf')]
            first = subprocess.run(args, capture_output=True, text=True, check=True).stdout
            again = subprocess.run(args, capture_output=True, text=True, check=True).stdout
            version = first.strip()
            self.assertRegex(version, r'^vc-[0-9a-f]{12}$')
            self.assertEqual(again, first)
            self.assertEqual(subprocess.run(['bash', str(script), '--version', str(archive)],
                                            capture_output=True, text=True, check=True).stdout, first)
            tree = tmp / 'root/usr/src' / f'mhvtl-{version}'
            self.assertTrue((tree / 'kernel/Makefile').is_file())
            self.assertIn(f'PACKAGE_VERSION="{version}"', (tree / 'dkms.conf').read_text())
            self.assertFalse(tree.with_name(tree.name + '.partial').exists())


if __name__ == '__main__':
    unittest.main()
