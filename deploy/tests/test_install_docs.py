from pathlib import Path
import re
import unittest

ROOT = Path(__file__).parents[2]
INSTALL_SH = (ROOT / 'deploy/install/install.sh').read_text()
UPDATE_PY = (ROOT / 'deploy/host/update.py').read_text()
INSTALL_DOC = (ROOT / 'docs/install.md').read_text()
DOCKER_DOC = (ROOT / 'docs/docker.md').read_text()


def section(text, heading):
    """The body of the `## heading` section of a Markdown text."""
    match = re.search(rf'^## {re.escape(heading)}\n(.*?)(?=^## |\Z)', text, re.S | re.M)
    assert match, heading
    return match.group(1)


def parser_options():
    """Every option the script's parse_options accepts, read from its case.

    Fails when a case pattern starts like an option but is not one of the forms
    understood here, so that a new form is never silently ignored.
    """
    body = INSTALL_SH.split('parse_options() {', 1)[1].split('\n}\n', 1)[0]
    found = set()
    for line in body.splitlines():
        text = line.strip()
        if not text.startswith('-'):
            continue
        match = re.match(r'(--[a-z-]+(?: \| --[a-z-]+)*)\)', text)
        if not match:
            raise AssertionError(f'unrecognised option pattern in parse_options: {text}')
        found.update(re.findall(r'--[a-z-]+', match.group(1)))
    return found


def table_first_column(text):
    return [row.group(1) for row in re.finditer(r'^\| (.+?) \|', text, re.M)]


class OptionsTableTests(unittest.TestCase):
    def test_parser_is_read(self):
        self.assertGreaterEqual(len(parser_options()), 9)

    def test_every_option_is_in_the_table_and_vice_versa(self):
        table = section(INSTALL_DOC, 'Options')
        documented = {option for cell in table_first_column(table)
                      for option in re.findall(r'`(--[a-z-]+)', cell)}
        self.assertEqual(documented, parser_options())


class ExitCodeTests(unittest.TestCase):
    def test_every_exit_code_is_in_the_table(self):
        used = set(re.findall(r'\bexit (\d+)\b', INSTALL_SH))
        used |= set(re.findall(r"EXIT_CODES = \{(.*?)\}", UPDATE_PY)[0].replace(' ', '').split(','))
        used = {code for item in used for code in re.findall(r'\d+$', item)}
        documented = set(table_first_column(section(INSTALL_DOC, 'Exit codes')))
        self.assertLessEqual(used, documented)

    def test_every_updater_result_is_documented(self):
        results = re.findall(r"'(\w+)': \d", re.search(r'EXIT_CODES = \{.*?\}', UPDATE_PY).group(0))
        self.assertEqual(len(results), 5)
        for result in results:
            self.assertIn(f'`{result}`', INSTALL_DOC)


class RollbackTests(unittest.TestCase):
    def test_markers_and_flags_are_documented(self):
        for name in ('update-in-progress.json', 'update-verified.json', '--resume-rollback', '--force-rollback'):
            self.assertIn(name, INSTALL_DOC)

    def test_marker_names_match_the_updater(self):
        self.assertIn("IN_PROGRESS = HOST_STATE + '/update-in-progress.json'", UPDATE_PY)
        self.assertIn("VERIFIED = HOST_STATE + '/update-verified.json'", UPDATE_PY)

    def test_every_backup_file_is_documented(self):
        names = re.findall(r"^BACKUP_\w+ = '([^']+)'", UPDATE_PY, re.M)
        names += ['isolation-qualified.json']
        for name in names:
            self.assertIn(name, INSTALL_DOC)


class MessageTests(unittest.TestCase):
    def test_the_half_installed_host_message_is_quoted_exactly(self):
        for name in ('HOST_LEFTOVER_CAUSE', 'HOST_LEFTOVER_RESUME'):
            message = re.search(rf'^{name}="(.*)"$', INSTALL_SH, re.M).group(1)
            self.assertGreater(len(message), 60)
            self.assertIn(message, INSTALL_DOC, name)
        for text in ('/etc/vaultcenter-host', 'vaultcenter_state', 'vaultcenter_media', 'your decision',
                     'Nothing has been deleted'):
            self.assertIn(text, re.search(r'^HOST_LEFTOVER_CAUSE="(.*)"$', INSTALL_SH, re.M).group(1))

    def test_the_guide_says_where_it_is_on_the_host(self):
        self.assertIn('/usr/local/share/vaultcenter/<version>/host/install.md', INSTALL_DOC)
        self.assertIn('install.md', INSTALL_SH.split('doc_path() {', 1)[1].split('\n}\n', 1)[0])

    def test_offline_rollback_is_documented(self):
        self.assertRegex(INSTALL_DOC, r'python3 /usr/local/share/vaultcenter/<[^>]+>/host/update\.py --resume-rollback')

    def test_sudo_journal_caution_sits_with_the_ps_caution(self):
        paragraph = next(part for part in INSTALL_DOC.split('\n\n') if '(`ps`)' in part)
        self.assertIn('sudo', paragraph)
        self.assertIn('system journal', paragraph)

    def test_sections_the_messages_name_exist(self):
        for heading in ('Keeping an unrecorded update', 'Manual procedure from a backup'):
            self.assertRegex(INSTALL_DOC, rf'(?m)^### {re.escape(heading)}$')
        for heading in ('The administrator account', 'The isolation receipt', 'Rollback'):
            self.assertRegex(INSTALL_DOC, rf'(?m)^## {re.escape(heading)}$')
        self.assertRegex(INSTALL_DOC, r'(?m)^### Resuming after an interruption$')
        for heading in ('Keeping an unrecorded update',):
            self.assertIn(f'"{heading}"', UPDATE_PY)
        for heading in ('The administrator account', 'The isolation receipt', 'Resuming after an interruption',
                        'Rollback'):
            self.assertRegex(INSTALL_SH, rf'\\?"{re.escape(heading)}\\?"')

    def test_what_this_wave_changed_is_documented(self):
        for text in ('linux-headers-cloud-amd64', 'vaultcenter-mhvtl.service', 'still_in_maintenance',
                     '--noproxy', 'Acquire::http::Proxy', 'X.Y.Z-<pre-release>', 'dkms install --force',
                     'modinfo -k', 'SIGHUP', 'other_kernels'):
            self.assertIn(text, INSTALL_DOC, text)


class DockerPointerTests(unittest.TestCase):
    def test_the_three_sections_point_to_the_installer(self):
        for heading in ('Preparing the Linux host', 'First start', 'Update and rollback'):
            first_paragraph = section(DOCKER_DOC, heading).strip().split('\n\n', 1)[0]
            self.assertIn('](install.md)', first_paragraph, heading)


class ProductNameTests(unittest.TestCase):
    def test_install_doc_never_names_the_former_product(self):
        self.assertNotIn('free' + 'zer', INSTALL_DOC.lower())


class LinkTests(unittest.TestCase):
    def test_relative_links_resolve(self):
        links = re.findall(r'\]\(([^)#\s]+)(?:#[^)]*)?\)', INSTALL_DOC)
        relative = [link for link in links if not re.match(r'[a-z]+:', link)]
        self.assertTrue(relative)
        for link in relative:
            self.assertTrue((ROOT / 'docs' / link).exists(), link)


if __name__ == '__main__':
    unittest.main()
