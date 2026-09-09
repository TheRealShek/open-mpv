"""Run real Git preparation and merge validation; only GitHub responses are fixtures."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).parents[2]
sys.path.insert(0, str(ROOT / 'packaging'))
spec = importlib.util.spec_from_file_location('prepare_integration', ROOT / 'packaging/prepare-release.py')
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class GitPreparationTests(unittest.TestCase):
    def test_prepare_merge_validate_after_main_moves(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            remote = root / 'remote.git'
            checkout = root / 'checkout'
            subprocess.run(['git', 'init', '--bare', '--initial-branch=main', str(remote)], check=True, capture_output=True)
            subprocess.run(['git', 'clone', str(remote), str(checkout)], check=True, capture_output=True)
            with contextlib.chdir(checkout), patch.dict(os.environ, {'GH_REPO': 'owner/repo'}):
                def git(*args):
                    return subprocess.check_output(['git', *args], text=True, stderr=subprocess.DEVNULL).strip()
                git('config', 'user.name', 'Fixture')
                git('config', 'user.email', 'fixture@example.invalid')
                for name in (*p.FILES, '.github/pull_request_template.md'):
                    destination = checkout / name
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(ROOT / name, destination)
                git('add', '.')
                git('commit', '-m', 'Initial source')
                git('push', 'origin', 'main')
                original_run = p.release.run
                def run(*args):
                    if args[:3] == ('gh', 'pr', 'create'):
                        self.assertEqual(args[args.index('--base') + 1], 'main')
                        return 'https://github.com/owner/repo/pull/1'
                    return original_run(*args)
                with patch.object(p.release, 'pages', return_value=[]), patch.object(p.release, 'release', return_value=None), patch.object(p.release, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                    p.prepare('9.0.0', 'Fix image & video handling')
                head = git('rev-parse', 'HEAD')
                git('switch', 'main')
                git('merge', '--no-ff', 'release/prepare-v9.0.0', '-m', 'Reviewed release preparation')
                merged = git('rev-parse', 'HEAD')
                Path('later.txt').write_text('Main advanced after the release merge.')
                git('add', 'later.txt')
                git('commit', '-m', 'Later unrelated change')
                git('push', 'origin', 'main')
                item = {'merged': True, 'base': {'ref': 'main'}, 'head': {'ref': 'release/prepare-v9.0.0', 'sha': head, 'repo': {'full_name': 'owner/repo'}}, 'merge_commit_sha': merged, 'html_url': 'https://github.com/owner/repo/pull/1'}
                def api(path):
                    if path == 'pulls/1':
                        return item
                    if path.startswith('commits/'):
                        return {'check_runs': [{'name': 'Required checks', 'app': {'slug': 'github-actions'}, 'conclusion': 'success'}]}
                    if path.startswith('git/matching-refs/'):
                        return []
                    raise AssertionError(path)
                with patch.object(p.release, 'api', side_effect=api), patch.object(p.release, 'release', return_value=None), patch.object(p, 'emit') as emit:
                    p.merged(1)
                    self.assertEqual(emit.call_args.kwargs['commit'], merged)
                    self.assertNotEqual(merged, git('rev-parse', 'main'))


class SigningSetupTests(unittest.TestCase):
    def test_missing_registration_reports_browser_action(self):
        with contextlib.chdir(ROOT), patch.dict(os.environ, {'GH_REPO': 'owner/repo'}), patch.object(p.release, 'run', return_value='[]'):
            with self.assertRaisesRegex(ValueError, 'https://github.com/settings/gpg/new'):
                p.check_signing_setup()

    def test_registered_public_key_and_verified_email(self):
        public = (ROOT / '.github/release-signing.asc').read_text()
        info = subprocess.check_output(['gpg', '--batch', '--with-colons', '--show-keys'], input=public, text=True, stderr=subprocess.DEVNULL)
        fingerprint = next(line.split(':')[9] for line in info.splitlines() if line.startswith('fpr:'))
        from email.utils import parseaddr
        email = parseaddr(next(line.split(':')[9] for line in info.splitlines() if line.startswith('uid:')))[1]
        key = {'key_id': fingerprint[-16:], 'raw_key': public, 'can_sign': True, 'revoked': False, 'emails': [{'email': email, 'verified': True}]}
        with contextlib.chdir(ROOT), patch.dict(os.environ, {'GH_REPO': 'owner/repo'}):
            with patch.object(p.release, 'run', return_value=json.dumps([key])):
                p.check_signing_setup()
            for change in ({'revoked': True}, {'can_sign': False}, {'emails': [{'email': email, 'verified': False}]}):
                with patch.object(p.release, 'run', return_value=json.dumps([key | change])):
                    with self.assertRaises(ValueError):
                        p.check_signing_setup()
