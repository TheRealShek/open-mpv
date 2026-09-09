"""Exercise source scripts only in temporary prefixes with a fixture RPM database."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).parents[2]
APP = 'io.github.TheRealShek.OpenMpv'


class SourceInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        for name in ('install.sh', 'uninstall.sh', 'LICENSE'):
            shutil.copy(ROOT / name, self.repo / name)
        shutil.copytree(ROOT / 'data', self.repo / 'data')
        (self.repo / 'packaging').mkdir()
        shutil.copy(ROOT / 'packaging/source-ownership.sh', self.repo / 'packaging/source-ownership.sh')
        binary = self.repo / 'target/release/open-mpv'
        binary.parent.mkdir(parents=True)
        binary.write_text('#!/bin/sh\nexit 0\n')
        binary.chmod(0o755)
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        rpm = self.tools / 'rpm'
        rpm.write_text('''#!/bin/sh
if [ "$1" = -qa ]; then exit "${RPM_BROKEN:-0}"; fi
[ "$1" = -qf ] || exit 2
[ "$3" = "${RPM_OWNED:-}" ]
''')
        rpm.chmod(0o755)
        for name in ('update-desktop-database', 'gtk-update-icon-cache'):
            tool = self.tools / name
            tool.write_text('#!/bin/sh\nexit 0\n')
            tool.chmod(0o755)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.env = os.environ | {'HOME': str(self.home), 'PATH': str(self.tools) + ':' + os.environ['PATH']}
        self.prefix = self.home / '.local'

    def invoke(self, script, *args, success=True):
        result = subprocess.run(['sh', str(self.repo / script), *args], env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_per_user_and_repeated_uninstall_preserve_configuration(self):
        config = self.home / '.config/open-mpv/config'
        config.parent.mkdir(parents=True)
        config.write_text('user settings')
        associations = self.home / '.config/mimeapps.list'
        associations.write_text('user defaults')
        self.invoke('install.sh', '--no-build')
        launcher = self.prefix / f'share/applications/{APP}.desktop'
        self.assertIn(str(self.prefix / 'bin'), launcher.read_text())
        self.invoke('uninstall.sh')
        self.invoke('uninstall.sh')
        self.assertFalse(launcher.exists())
        self.assertFalse((self.prefix / 'bin/open-mpv').exists())
        self.assertEqual(config.read_text(), 'user settings')
        self.assertEqual(associations.read_text(), 'user defaults')

    def test_late_conflict_prevents_all_writes_and_removals(self):
        self.invoke('install.sh', '--no-build')
        binary = self.prefix / 'bin/open-mpv'
        binary.write_text('old build')
        self.env['RPM_OWNED'] = str(self.prefix / 'share/licenses/open-mpv/LICENSE')
        result = self.invoke('install.sh', '--no-build', success=False)
        self.assertIn('DNF', result.stderr)
        self.invoke('uninstall.sh', success=False)
        self.assertEqual(binary.read_text(), 'old build')
        self.assertTrue((self.prefix / f'share/applications/{APP}.desktop').exists())

    def test_custom_prefix_and_staging_ignore_live_ownership(self):
        prefix = self.root / 'custom'
        self.invoke('install.sh', '--no-build', '--prefix', str(prefix))
        self.invoke('uninstall.sh', '--prefix', str(prefix))
        self.env['RPM_OWNED'] = '/usr/bin/open-mpv'
        stage = self.root / 'stage'
        self.invoke('install.sh', '--no-build', '--prefix', '/usr', '--destdir', str(stage))
        self.assertTrue((stage / 'usr/bin/open-mpv').exists())
        self.env['RPM_OWNED'] = str(stage / 'usr/bin/open-mpv')
        self.invoke('install.sh', '--no-build', '--prefix', '/usr', '--destdir', str(stage), success=False)

    def test_alias_and_missing_owned_destination_are_protected(self):
        actual = self.root / 'actual'
        actual.mkdir()
        alias = self.root / 'alias'
        alias.symlink_to(actual, target_is_directory=True)
        self.env['RPM_OWNED'] = str(actual / 'bin/open-mpv')
        self.invoke('install.sh', '--no-build', '--prefix', str(alias), success=False)
        self.assertFalse((actual / 'bin').exists())
        self.invoke('uninstall.sh', '--prefix', str(alias), success=False)

    def test_broken_rpm_database_fails_before_mutation(self):
        self.env['RPM_BROKEN'] = '1'
        self.invoke('install.sh', '--no-build', success=False)
        self.assertFalse(self.prefix.exists())


if __name__ == '__main__':
    unittest.main()
