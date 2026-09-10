"""Release API fixtures never mutate production releases."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('release', Path(__file__).parents[1] / 'release.py')
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)


class ReleaseTests(unittest.TestCase):
    def test_published_refusal(self):
        for immutable in (True, False):
            with self.assertRaises(ValueError):
                r.require_draft({'draft': False, 'immutable': immutable})
        r.require_draft({'draft': True})

    def test_invalid_versions_before_api(self):
        with patch.object(r, 'api') as api:
            for tag in ('1.2.3', 'v01.2.3', 'v1.2.3-rc1', 'v1.2', '../main', 'v1.2.3\n'):
                with self.assertRaises(ValueError):
                    r.validate(tag)
            api.assert_not_called()

    def test_tag_conflicts_and_signature(self):
        obj = {'tag': 'v1.2.3', 'object': {'type': 'commit', 'sha': 'commit'},
               'verification': {'verified': True}}
        for changed in ({'verification': {'verified': False}}, {'tag': 'v1.2.4'},
                        {'object': {'type': 'tag', 'sha': 'nested'}}):
            with patch.object(r, 'api', side_effect=[{'object': {'type': 'tag', 'sha': 'tag'}}, obj | changed]):
                with self.assertRaises(ValueError):
                    r.validate('v1.2.3', expected_commit='commit')
        with patch.object(r, 'api', side_effect=[{'object': {'type': 'tag', 'sha': 'tag'}}, obj]):
            with self.assertRaises(ValueError):
                r.validate('v1.2.3', expected_commit='different')
        with patch.object(r, 'api', side_effect=[{'object': {'type': 'tag', 'sha': 'tag'}}, obj]), patch.object(r, 'run', return_value='[package]\nversion="1.2.3"'), patch.object(r, 'release', return_value=None), patch.dict(r.os.environ, {'GH_REPO': 'owner/repo'}):
            self.assertEqual(r.validate('v1.2.3', 'commit', 'tag'),
                             {'tag': 'v1.2.3', 'commit_sha': 'commit', 'tag_sha': 'tag'})
        with patch.object(r, 'api', return_value={'object': {'type': 'commit'}}):
            with self.assertRaises(ValueError):
                r.validate('v1.2.3')

    def test_pagination(self):
        with patch.object(r, 'api', side_effect=[list(range(100)), [100]]) as api:
            self.assertEqual(list(r.pages('releases')), list(range(101)))
            self.assertIn('page=2', api.call_args.args[0])

    def test_draft_lookup_does_not_depend_on_release_listing(self):
        item = {'id': 7, 'tag_name': 'v1.2.3', 'draft': True}
        lookup = {'data': {'repository': {'release': {'databaseId': 7}}}}
        with patch.dict(r.os.environ, {'GH_REPO': 'owner/repo'}), patch.object(r, 'pages', return_value=[]) as pages, patch.object(r, 'run', return_value=json.dumps(lookup)), patch.object(r, 'api', return_value=item) as api:
            self.assertEqual(r.release('v1.2.3'), item)
            api.assert_called_once_with('releases/7')
            pages.assert_not_called()

    def test_release_lookup_distinguishes_absence_from_failure(self):
        with patch.dict(r.os.environ, {'GH_REPO': 'owner/repo'}), patch.object(r, 'api') as api:
            with patch.object(r, 'run', return_value='{"data":{"repository":{"release":null}}}'):
                self.assertIsNone(r.release('v1.2.3'))
                api.assert_not_called()
            with patch.object(r, 'run', side_effect=r.subprocess.CalledProcessError(1, 'gh')):
                with self.assertRaises(r.subprocess.CalledProcessError):
                    r.release('v1.2.3')
            with patch.object(r, 'run', return_value='{"data":{"repository":{"release":{"databaseId":7}}}}'):
                api.return_value = {'id': 7, 'tag_name': 'v1.2.4', 'draft': True}
                with self.assertRaisesRegex(ValueError, 'tag changed'):
                    r.release('v1.2.3')

    def test_assets_compared_before_upload(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            for name in r.ASSETS:
                (directory / name).write_text(name)
            asset = {'id': 1, 'name': r.RPM, 'state': 'uploaded'}
            def download(_, destination):
                destination.write_text(r.RPM)
            with patch.object(r, 'download', side_effect=download):
                self.assertEqual(r.asset_plan([asset], directory), ['release-source.json', 'SHA256SUMS'])
            with patch.object(r, 'download', side_effect=lambda _, p: p.write_text('different')):
                with self.assertRaises(ValueError):
                    r.asset_plan([asset], directory)
            with self.assertRaises(ValueError):
                r.asset_plan([asset | {'state': 'starter'}], directory)

    def test_restore_identity_checksums_and_partial_uploads(self):
        identity = {'tag': 'v1.2.3', 'commit_sha': 'commit', 'tag_sha': 'tag'}
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            contents = {'release-source.json': json.dumps(identity), r.RPM: 'rpm'}
            (directory / 'fixture').write_text('rpm')
            contents['SHA256SUMS'] = f'{r.digest(directory / "fixture")}  {r.RPM}\n'
            assets = [{'name': name, 'id': i, 'state': 'uploaded'} for i, name in enumerate(r.ASSETS)]
            def download(asset, destination):
                destination.write_text(contents[asset['name']])
            with patch.object(r, 'validate', return_value=identity), patch.object(r, 'release', return_value={'id': 1}), patch.object(r, 'download', side_effect=download):
                with patch.object(r, 'pages', return_value=assets):
                    r.restore('v1.2.3', 'commit', 'tag', directory)
                    self.assertEqual((directory / r.RPM).read_text(), 'rpm')
                    contents['SHA256SUMS'] = 'bad'
                    with self.assertRaises(ValueError):
                        r.restore('v1.2.3', 'commit', 'tag', directory)
                with patch.object(r, 'pages', return_value=assets[:2]):
                    with self.assertRaises(ValueError):
                        r.restore('v1.2.3', 'commit', 'tag', directory)

    def test_predecessor_uses_rpm_order_and_excludes_incompatible(self):
        releases = [{'id': i, 'tag_name': f'v{i}.0.0', 'draft': False, 'prerelease': False}
                    for i in range(1, 7)]
        evrs = [('0', '1.9', '1.fc44'), ('0', '1.10', '1.fc44'),
                ('0', '2.0', '1.fc44'), ('1', '1.0', '1.fc44'),
                ('0', '1.11', '1.fc43'), ('0', '1.8', '1.fc44')]
        def pages(path):
            if path == 'releases':
                return releases
            return [{'id': int(path.split('/')[1]), 'name': r.RPM, 'state': 'uploaded'}]
        def identity(path):
            if path.name == r.RPM and path.parent == directory:
                return ['open-mpv', 'x86_64'], ('0', '2.0', '1.fc44')
            return ['open-mpv', 'x86_64'], evrs[int(path.read_text()) - 1]
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            with patch.object(r, 'pages', side_effect=pages), patch.object(r, 'download', side_effect=lambda a, p: p.write_text(str(a['id']))), patch.object(r, 'rpm_identity', side_effect=identity):
                r.predecessor('v2.0.0-candidate', directory)
                self.assertEqual((directory / 'previous.rpm').read_text(), '2')
            with patch.object(r, 'pages', return_value=[]), patch.object(r, 'rpm_identity', return_value=(['open-mpv', 'x86_64'], ('0', '2', '1.fc44'))):
                r.predecessor('v2.0.0', directory)
                self.assertFalse((directory / 'previous.rpm').exists())

    def test_nonexistent_tag_and_shared_concurrency(self):
        with patch.object(r, 'api', side_effect=r.subprocess.CalledProcessError(1, 'gh')):
            with self.assertRaises(r.subprocess.CalledProcessError):
                r.validate('v1.2.3')
        workflow = (Path(__file__).parents[2] / '.github/workflows/release.yml').read_text()
        self.assertIn('group: release-${{ inputs.tag || github.ref_name }}', workflow)
        self.assertIn('cancel-in-progress: false', workflow)

    def test_upload_preserves_notes_and_never_clobbers(self):
        identity = {'tag': 'v1.2.3', 'commit_sha': 'commit', 'tag_sha': 'tag'}
        item = {'id': 1, 'draft': True, 'html_url': 'draft', 'body': 'Edited notes', 'name': 'Edited title'}
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / r.RPM).write_text('rpm')
            (directory / 'SHA256SUMS').write_text(f'{r.digest(directory / r.RPM)}  {r.RPM}\n')
            with patch.dict(r.os.environ, {'GH_REPO': 'owner/repo'}), patch.object(r, 'validate', return_value=identity), patch.object(r, 'release', return_value=item), patch.object(r, 'api', return_value=item), patch.object(r, 'pages', return_value=[]), patch.object(r, 'run') as run:
                r.upload('v1.2.3', 'commit', 'tag', directory)
                self.assertEqual(run.call_count, 3)
                for call in run.call_args_list:
                    self.assertEqual(call.args[:4], ('gh', 'api', '--method', 'POST'))
                    self.assertTrue(call.args[4].startswith('https://uploads.github.com/repos/owner/repo/releases/1/assets?name='))
                    self.assertIn('--input', call.args)
                run.reset_mock()
                assets = [{'name': name, 'state': 'uploaded', 'id': i} for i, name in enumerate(r.ASSETS)]
                with patch.object(r, 'pages', return_value=assets), patch.object(r, 'download', side_effect=lambda a, p: p.write_bytes((directory / a['name']).read_bytes())):
                    r.upload('v1.2.3', 'commit', 'tag', directory)
                run.assert_not_called()

    def test_create_draft_when_release_list_has_not_updated(self):
        identity = {'tag': 'v1.2.3', 'commit_sha': 'commit', 'tag_sha': 'tag'}
        item = {'id': 7, 'tag_name': 'v1.2.3', 'draft': True, 'html_url': 'draft'}
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / r.RPM).write_text('rpm')
            (directory / 'SHA256SUMS').write_text(f'{r.digest(directory / r.RPM)}  {r.RPM}\n')
            with patch.dict(r.os.environ, {'GH_REPO': 'owner/repo'}), patch.object(r, 'validate', return_value=identity), patch.object(r, 'release', return_value=None), patch.object(r, 'api', return_value=item), patch.object(r, 'pages', return_value=[]), patch.object(r, 'run', return_value=json.dumps(item)) as run:
                r.upload('v1.2.3', 'commit', 'tag', directory)
                creation = run.call_args_list[0].args
                self.assertEqual(creation[:5], ('gh', 'api', '--method', 'POST', 'repos/owner/repo/releases'))
                self.assertIn('draft=true', creation)
                self.assertIn('target_commitish=commit', creation)
                self.assertIn('generate_release_notes=true', creation)
                uploads = [call for call in run.call_args_list if call.args[4].startswith('https://uploads.github.com/repos/owner/repo/releases/7/assets?name=')]
                self.assertEqual(len(uploads), 3)

    def test_failed_draft_creation_does_not_retry_or_upload(self):
        identity = {'tag': 'v1.2.3', 'commit_sha': 'commit', 'tag_sha': 'tag'}
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / r.RPM).write_text('rpm')
            (directory / 'SHA256SUMS').write_text(f'{r.digest(directory / r.RPM)}  {r.RPM}\n')
            # An ambiguous network failure or a concurrent creation must stop;
            # a later run can discover the draft without creating another one.
            with patch.dict(r.os.environ, {'GH_REPO': 'owner/repo'}), patch.object(r, 'validate', return_value=identity), patch.object(r, 'release', return_value=None), patch.object(r, 'run', side_effect=r.subprocess.CalledProcessError(1, 'gh')) as run:
                with self.assertRaises(r.subprocess.CalledProcessError):
                    r.upload('v1.2.3', 'commit', 'tag', directory)
                self.assertEqual(run.call_count, 1)


if __name__ == '__main__':
    unittest.main()
