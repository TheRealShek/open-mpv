"""Metadata preparation and merged-commit safeguards using local fixtures."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
import tomllib
import xml.etree.ElementTree as ET

ROOT = Path(__file__).parents[2]
sys.path.insert(0, str(ROOT / 'packaging'))
spec = importlib.util.spec_from_file_location('prepare', ROOT / 'packaging/prepare-release.py')
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.files = {name: (ROOT / name).read_text() for name in p.FILES}

    def test_versions_and_descriptions(self):
        for version in ('v1.2.3', '01.2.3', '1.2', '1.2.3-rc1', '1.2.3\n'):
            with self.assertRaises(ValueError):
                p.edits(self.files, version, 'Release', '2026-09-09')
        for description in ('', ' ', 'bad\nline', 'bad\x00text', 'x' * 1001):
            with self.assertRaises(ValueError):
                p.edits(self.files, '9.0.0', description, '2026-09-09')
        current = tomllib.loads(self.files['Cargo.toml'])['package']['version']
        with self.assertRaises(ValueError):
            p.edits(self.files, current, 'Release', '2026-09-09')

    def test_preserves_dependencies_history_and_encodes_text(self):
        description = 'Fix <image> & "video"; 100% %{lua:unsafe()} `shell` $HOME'
        output = p.edits(self.files, '9.0.0', description, '2026-09-09')
        old = tomllib.loads(self.files['Cargo.lock'])
        new = tomllib.loads(output['Cargo.lock'])
        for package in old['package']:
            if package['name'] == 'open-mpv':
                package['version'] = '9.0.0'
        self.assertEqual(old, new)
        old_toml = tomllib.loads(self.files['Cargo.toml'])
        old_toml['package']['version'] = '9.0.0'
        self.assertEqual(old_toml, tomllib.loads(output['Cargo.toml']))
        self.assertIn(self.files[p.FILES[2]].split('%changelog\n')[1], output[p.FILES[2]])
        self.assertIn('%%{lua:unsafe()}', output[p.FILES[2]])
        self.assertEqual(ET.fromstring(output[p.FILES[3]]).find('releases/release/description/p').text, description)
        self.assertIn(self.files[p.FILES[3]].split('  <releases>\n')[1], output[p.FILES[3]])

    def test_competing_preparation_refused_and_duplicate_untouched(self):
        item = {'head': {'ref': 'release/prepare-v9.0.0', 'repo': {'full_name': 'owner/repo'}},
                'state': 'open', 'html_url': 'existing-pr'}
        with patch.dict(p.os.environ, {'GH_REPO': 'owner/repo'}), patch.object(p.release, 'release', return_value=None), patch.object(p.release, 'pages', return_value=[item]), patch.object(p.release, 'run') as run:
            with self.assertRaises(ValueError):
                p.prepare('9.0.1', 'Other release')
            p.prepare('9.0.0', 'Release')
            run.assert_not_called()

    def test_unmerged_and_foreign_pr_cannot_sign(self):
        with patch.object(p.release, 'api', return_value={'merged': False}):
            with self.assertRaises(ValueError):
                p.merged(1)

    def test_secret_isolation_and_explicit_packaging(self):
        workflow = (ROOT / '.github/workflows/prepare-release.yml').read_text()
        self.assertEqual(workflow.count('secrets.RELEASE_SIGNING_KEY'), 1)
        self.assertIn('environment: release-signing', workflow)
        self.assertIn('uses: ./.github/workflows/release.yml', workflow)
        self.assertNotIn('secrets: inherit', workflow)
        sign = workflow.split('  sign:\n')[1].split('  package:\n')[0]
        self.assertNotIn('cargo ', sign)
        self.assertNotIn('refs/heads/main"', sign)
        self.assertIn('"$TAG" "$COMMIT"', sign)


if __name__ == '__main__':
    unittest.main()
