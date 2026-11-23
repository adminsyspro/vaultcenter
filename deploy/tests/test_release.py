import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).parents[2]
CHECK = ROOT / 'deploy/tests/check_release_version.sh'
RELEASE = ROOT / 'release.sh'


def write_version_files(directory, cargo='0.1.0', package='0.1.0', changelog='0.1.0'):
    directory = Path(directory)
    (directory / 'ui').mkdir(exist_ok=True)
    (directory / 'Cargo.toml').write_text(
        '[workspace]\nmembers = []\n\n[workspace.package]\nedition = "2024"\n'
        f'version = "{cargo}"\n')
    (directory / 'ui/package.json').write_text(
        f'{{\n  "name": "ui",\n  "version": "{package}"\n}}\n')
    sections = f'## [{changelog}] - 2026-10-23\n' if changelog else ''
    (directory / 'CHANGELOG.md').write_text(
        f'# Changelog\n\n## [Unreleased]\n\n{sections}')


def git_env():
    env = dict(os.environ)
    env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
               GIT_AUTHOR_NAME='Test', GIT_AUTHOR_EMAIL='test@example.invalid',
               GIT_COMMITTER_NAME='Test', GIT_COMMITTER_EMAIL='test@example.invalid')
    return env


class CheckReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True, env=git_env())
        (self.root / 'deploy/tests').mkdir(parents=True)
        shutil.copy(CHECK, self.root / 'deploy/tests/check_release_version.sh')
        write_version_files(self.root)

    def run_check(self, *args):
        return subprocess.run(
            ['bash', 'deploy/tests/check_release_version.sh', *args],
            cwd=self.root, env=git_env(), capture_output=True, text=True)

    def test_without_argument_prints_the_version(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '0.1.0\n')

    def test_package_json_disagreeing_fails(self):
        write_version_files(self.root, package='0.1.1')
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(result.stderr.strip(), '')

    def test_matching_tag_passes(self):
        result = self.run_check('v0.1.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '0.1.0\n')

    def test_malformed_tags_fail(self):
        for tag in ['v0.1', '0.1.0', 'v0.1.0-rc1', 'v0.01.0']:
            with self.subTest(tag=tag):
                self.assertNotEqual(self.run_check(tag).returncode, 0)

    def test_tag_different_from_cargo_fails(self):
        self.assertNotEqual(self.run_check('v0.2.0').returncode, 0)

    def test_tag_without_changelog_section_fails(self):
        write_version_files(self.root, changelog=None)
        self.assertNotEqual(self.run_check('v0.1.0').returncode, 0)

    def test_release_candidate_needs_no_changelog_section(self):
        write_version_files(self.root, cargo='0.2.0-rc.1', package='0.2.0-rc.1', changelog=None)
        self.assertEqual(self.run_check().stdout, '0.2.0-rc.1\n')
        result = self.run_check('v0.2.0-rc.1')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '0.2.0-rc.1\n')

    def test_malformed_release_candidates_fail(self):
        for version in ['0.2.0-rc.0', '0.2.0-rc.01', '0.2.0-beta.1', '0.2.0-rc']:
            with self.subTest(version=version):
                write_version_files(self.root, cargo=version, package=version, changelog=None)
                self.assertNotEqual(self.run_check().returncode, 0)
                self.assertNotEqual(self.run_check('v' + version).returncode, 0)

    def test_release_candidate_tag_must_match_cargo(self):
        self.assertNotEqual(self.run_check('v0.1.0-rc.1').returncode, 0)


CARGO_LOCK = (
    'version = 4\n\n'
    '[[package]]\nname = "app-one"\nversion = "0.1.0"\n\n'
    '[[package]]\nname = "app-two"\nversion = "0.1.0"\ndependencies = [\n "foo",\n]\n\n'
    '[[package]]\nname = "foo"\nversion = "0.1.0"\n'
    'source = "registry+https://github.com/rust-lang/crates.io-index"\n')


def git(cwd, *args, check=True):
    return subprocess.run(['git', *args], cwd=cwd, env=git_env(), check=check,
                          capture_output=True, text=True)


class ReleaseScriptTests(unittest.TestCase):
    def setUp(self):
        self.build()

    def build(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.origin = base / 'origin.git'
        self.root = base / 'work'
        git(base, 'init', '-q', '--bare', '-b', 'main', str(self.origin))
        git(base, 'init', '-q', '-b', 'main', str(self.root))
        git(self.root, 'remote', 'add', 'origin', str(self.origin))
        (self.root / 'deploy/tests').mkdir(parents=True)
        shutil.copy(CHECK, self.root / 'deploy/tests/check_release_version.sh')
        shutil.copy(RELEASE, self.root / 'release.sh')
        write_version_files(self.root, changelog=None)
        (self.root / 'Cargo.lock').write_text(CARGO_LOCK)
        lock = {'name': 'ui', 'version': '0.1.0', 'lockfileVersion': 3,
                'packages': {'': {'name': 'ui', 'version': '0.1.0'},
                             'node_modules/foo': {'version': '0.1.0'}}}
        (self.root / 'ui/package-lock.json').write_text(json.dumps(lock, indent=2) + '\n')
        (self.root / 'CHANGELOG.md').write_text(
            '# Changelog\n\n## [Unreleased]\n\n### Ajouts\n\n- Première version.\n')
        git(self.root, 'add', '-A')
        git(self.root, 'commit', '-q', '-m', 'Initial')
        git(self.root, 'push', '-q', 'origin', 'main')

    def run_release(self, version, answer='n'):
        return subprocess.run(['bash', 'release.sh', version], cwd=self.root,
                              env=git_env(), input=answer + '\n',
                              capture_output=True, text=True)

    def snapshot(self):
        files = {str(p.relative_to(self.root)): p.read_bytes()
                 for p in self.root.rglob('*')
                 if p.is_file() and '.git' not in p.relative_to(self.root).parts}
        return (git(self.root, 'rev-parse', 'HEAD').stdout,
                git(self.root, 'tag').stdout, files)

    def read_json(self, name):
        return json.loads((self.root / name).read_text())

    def test_refusals_leave_repository_untouched(self):
        def other_branch():
            git(self.root, 'checkout', '-q', '-b', 'other')

        def modified():
            (self.root / 'Cargo.lock').write_text('changed\n')

        def untracked():
            (self.root / 'stray.txt').write_text('x\n')

        def origin_ahead():
            clone = Path(self.temp.name) / 'second'
            git(Path(self.temp.name), 'clone', '-q', str(self.origin), str(clone))
            (clone / 'other.txt').write_text('x\n')
            git(clone, 'add', '-A')
            git(clone, 'commit', '-q', '-m', 'Ahead')
            git(clone, 'push', '-q', 'origin', 'main')

        def local_tag():
            git(self.root, 'tag', 'v0.2.0')

        def remote_tag():
            git(self.root, 'tag', 'v0.2.0')
            git(self.root, 'push', '-q', 'origin', 'v0.2.0')
            git(self.root, 'tag', '-d', 'v0.2.0')

        def newer_tag():
            git(self.root, 'tag', 'v0.3.0')

        def commit_changelog(text):
            (self.root / 'CHANGELOG.md').write_text(text)
            git(self.root, 'commit', '-q', '-am', 'Changelog')
            git(self.root, 'push', '-q', 'origin', 'main')

        def hide_untracked():
            git(self.root, 'config', 'status.showUntrackedFiles', 'no')
            untracked()

        form, nonempty = 'not of the form', "no non-empty '## [Unreleased]'"
        cases = [
            ('v0.2.0', 'v0.2.0', None, form), ('0.2', '0.2', None, form),
            ('rc', '0.2.0-rc1', None, form), ('leading zero', '0.02.0', None, form),
            ('branch', '0.2.0', other_branch, 'not main'),
            ('modified', '0.2.0', modified, 'modifications'),
            ('untracked', '0.2.0', untracked, 'modifications'),
            ('untracked hidden by config', '0.2.0', hide_untracked, 'modifications'),
            ('origin ahead', '0.2.0', origin_ahead, 'differs from origin/main'),
            ('local tag', '0.2.0', local_tag, 'already exists locally'),
            ('remote tag', '0.2.0', remote_tag, 'already exists on origin'),
            ('older than tag', '0.2.0', newer_tag, 'not greater than the latest tag'),
            ('empty title only', '0.2.0', lambda: commit_changelog(
                '# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-01-01\n\n- Old.\n'),
             nonempty),
            ('empty subheadings', '0.2.0', lambda: commit_changelog(
                '# Changelog\n\n## [Unreleased]\n\n### Ajouts\n\n### Corrections\n\n'
                '## [0.1.0] - 2026-01-01\n\n- Old.\n'), nonempty),
            ('absent', '0.2.0', lambda: commit_changelog('# Changelog\n\n- Old.\n'),
             nonempty),
        ]
        for name, version, setup, reason in cases:
            with self.subTest(name):
                self.build()
                if setup:
                    setup()
                before = self.snapshot()
                result = self.run_release(version, 'y')
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(reason, result.stderr)
                if name == 'untracked':
                    self.assertIn('stray.txt', result.stderr)
                self.assertEqual(self.snapshot(), before)

    def test_unreachable_origin_is_a_refusal(self):
        moved = self.origin.with_name('moved.git')
        self.origin.rename(moved)
        before = self.snapshot()
        result = self.run_release('0.2.0', 'y')
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('git fetch origin failed', result.stderr)
        self.assertEqual(self.snapshot(), before)

    def reject_pushes(self, pattern):
        hook = self.origin / 'hooks/pre-receive'
        hook.write_text('#!/bin/sh\nwhile read old new ref; do\n'
                        f'  case "$ref" in {pattern}) echo rejected >&2; exit 1;; esac\n'
                        'done\n')
        hook.chmod(0o755)

    def test_rejected_main_push_explains_the_undo(self):
        self.reject_pushes('refs/heads/main')
        before_main = git(self.origin, 'rev-parse', 'main').stdout
        result = self.run_release('0.2.0', 'y')
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('git tag -d v0.2.0 && git reset --hard HEAD~1', result.stderr)
        self.assertEqual(git(self.origin, 'rev-parse', 'main').stdout, before_main)
        self.assertEqual(git(self.origin, 'tag').stdout, '')
        self.assertEqual(git(self.root, 'tag').stdout, 'v0.2.0\n')

    def test_rejected_tag_push_says_to_push_the_tag(self):
        self.reject_pushes('refs/tags/*')
        result = self.run_release('0.2.0', 'y')
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('git push origin v0.2.0', result.stderr)
        self.assertNotIn('reset --hard', result.stderr)
        self.assertEqual(git(self.origin, 'rev-parse', 'main').stdout,
                         git(self.root, 'rev-parse', 'HEAD').stdout)
        self.assertEqual(git(self.origin, 'tag').stdout, '')

    def index_state(self):
        return git(self.root, 'status', '--porcelain', '--untracked-files=normal').stdout

    def test_failures_after_writing_restore_the_start(self):
        def failing_hook():
            hook = self.root / '.git/hooks/pre-commit'
            hook.write_text('#!/bin/sh\nexit 1\n')
            hook.chmod(0o755)

        def failing_tag():
            git(self.root, 'config', 'tag.gpgSign', 'true')
            git(self.root, 'config', 'gpg.program', 'false')

        for name, setup in [('pre-commit hook', failing_hook), ('tag', failing_tag)]:
            with self.subTest(name):
                self.build()
                setup()
                before = self.snapshot()
                result = self.run_release('0.2.0', 'y')
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn('repository restored', result.stderr)
                self.assertEqual(self.snapshot(), before)
                self.assertEqual(self.index_state(), '')
                self.assertEqual(git(self.origin, 'tag').stdout, '')

    def test_release_prepares_commit_tag_and_changelog(self):
        result = self.run_release('0.2.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('version = "0.2.0"', (self.root / 'Cargo.toml').read_text())
        package = self.read_json('ui/package.json')
        self.assertEqual(package['version'], '0.2.0')
        lock = self.read_json('ui/package-lock.json')
        self.assertEqual(lock['version'], '0.2.0')
        self.assertEqual(lock['packages']['']['version'], '0.2.0')
        today = datetime.date.today().isoformat()
        self.assertEqual(
            (self.root / 'CHANGELOG.md').read_text(),
            '# Changelog\n\n## [Unreleased]\n\n'
            f'## [0.2.0] - {today}\n\n### Ajouts\n\n- Première version.\n')
        self.assertEqual(git(self.root, 'log', '-1', '--format=%s').stdout, 'Release v0.2.0\n')
        self.assertEqual(git(self.root, 'tag', '--points-at', 'HEAD').stdout, 'v0.2.0\n')
        self.assertEqual(git(self.root, 'cat-file', '-t', 'v0.2.0').stdout, 'tag\n')
        self.assertEqual(
            git(self.root, 'tag', '-l', '--format=%(contents:subject)', 'v0.2.0').stdout,
            'VaultCenter 0.2.0\n')
        self.assertEqual(git(self.root, 'status', '--porcelain').stdout, '')
        self.assertEqual(git(self.origin, 'tag').stdout, '')
        self.assertNotEqual(git(self.origin, 'rev-parse', 'main').stdout,
                            git(self.root, 'rev-parse', 'HEAD').stdout)
        self.assertIn('git tag -d v0.2.0 && git reset --hard HEAD~1', result.stdout)

    def test_lock_only_workspace_crates(self):
        self.assertEqual(self.run_release('0.2.0').returncode, 0)
        lock = (self.root / 'Cargo.lock').read_text()
        self.assertIn('name = "app-one"\nversion = "0.2.0"', lock)
        self.assertIn('name = "app-two"\nversion = "0.2.0"', lock)
        self.assertIn('name = "foo"\nversion = "0.1.0"', lock)
        packages = self.read_json('ui/package-lock.json')['packages']
        self.assertEqual(packages['node_modules/foo']['version'], '0.1.0')

    def test_yes_pushes_main_and_tag(self):
        result = self.run_release('0.2.0', 'y')
        self.assertEqual(result.returncode, 0, result.stderr)
        head = git(self.root, 'rev-parse', 'HEAD').stdout
        self.assertEqual(git(self.origin, 'rev-parse', 'main').stdout, head)
        self.assertEqual(git(self.origin, 'tag').stdout, 'v0.2.0\n')

    def test_release_candidate_versions_files_and_keeps_the_changelog(self):
        changelog = (self.root / 'CHANGELOG.md').read_text()
        result = self.run_release('0.2.0-rc.1', 'y')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('version = "0.2.0-rc.1"', (self.root / 'Cargo.toml').read_text())
        self.assertEqual(self.read_json('ui/package.json')['version'], '0.2.0-rc.1')
        lock = self.read_json('ui/package-lock.json')
        self.assertEqual(lock['packages']['']['version'], '0.2.0-rc.1')
        self.assertIn('name = "app-one"\nversion = "0.2.0-rc.1"', (self.root / 'Cargo.lock').read_text())
        self.assertEqual((self.root / 'CHANGELOG.md').read_text(), changelog)
        self.assertEqual(git(self.root, 'log', '-1', '--format=%s').stdout, 'Release v0.2.0-rc.1\n')
        self.assertEqual(
            git(self.root, 'tag', '-l', '--format=%(contents:subject)', 'v0.2.0-rc.1').stdout,
            'VaultCenter 0.2.0-rc.1\n')
        self.assertEqual(git(self.origin, 'tag').stdout, 'v0.2.0-rc.1\n')

    def test_versions_are_ordered_with_release_candidates_first(self):
        # (existing tags, requested version, accepted)
        cases = [
            (['v0.2.0-rc.1'], '0.2.0', True),
            (['v0.2.0-rc.1'], '0.2.0-rc.2', True),
            (['v0.2.0-rc.9'], '0.2.0-rc.10', True),
            (['v0.1.0'], '0.2.0-rc.1', True),
            (['v0.2.0'], '0.2.0-rc.1', False),
            (['v0.2.0-rc.2'], '0.2.0-rc.1', False),
            (['v0.2.0-rc.1'], '0.2.0-rc.1', False),
            (['v0.2.0-rc.1', 'v0.3.0'], '0.2.0', False),
        ]
        for tags, version, accepted in cases:
            with self.subTest(tags=tags, version=version):
                self.build()
                for tag in tags:
                    git(self.root, 'tag', tag)
                result = self.run_release(version)
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertTrue('not greater than the latest tag' in result.stderr
                                    or 'already exists locally' in result.stderr, result.stderr)

    def test_final_release_after_a_candidate_dates_the_changelog(self):
        git(self.root, 'tag', 'v0.2.0-rc.1')
        self.assertEqual(self.run_release('0.2.0').returncode, 0)
        self.assertIn(f'## [0.2.0] - {datetime.date.today().isoformat()}',
                      (self.root / 'CHANGELOG.md').read_text())

    def test_first_release_changes_only_changelog(self):
        result = self.run_release('0.1.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        changed = git(self.root, 'show', '--name-only', '--format=', 'HEAD').stdout
        self.assertEqual(changed, 'CHANGELOG.md\n')


class PublishedTagsTests(unittest.TestCase):
    def test_release_candidates_move_no_shared_tag(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        guard = "!contains(github.ref_name, '-')"
        for line in ('type=semver,pattern={{major}}.{{minor}},enable=${{ ' + guard + ' }}',
                     'type=semver,pattern={{major}},enable=${{ ' + guard + ' }}',
                     "type=raw,value=latest,enable=${{ startsWith(github.ref, 'refs/tags/v') && "
                     + guard + ' }}'):
            with self.subTest(line=line):
                self.assertIn(line, workflow)


if __name__ == '__main__':
    unittest.main()
