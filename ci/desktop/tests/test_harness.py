import argparse
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zlib
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import fixtures
import host


class HarnessTests(unittest.TestCase):
    def test_bootstrap_failure_is_retained_before_guest_start(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            docker = directory / 'docker'
            docker.write_text('#!/bin/sh\ncase "$1" in build) exit 17 ;; container) exit 1 ;; esac\n')
            docker.chmod(0o755)
            artifacts = directory / 'evidence'
            command = ['bash', str(Path(host.__file__).with_name('run.sh')), 'gnome', str(artifacts)]
            result = subprocess.run(command, env={**os.environ, 'PATH': f'{directory}:{os.environ["PATH"]}'},
                                    capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 17)
            status = json.loads((artifacts / 'bootstrap-result.json').read_text())
            self.assertEqual(status, {'status': 'failed', 'exit_code': result.returncode})
            self.assertFalse((artifacts / 'guest/result.json').exists())

    def test_bootstrap_rejects_stale_artifacts_without_overwriting(self):
        with tempfile.TemporaryDirectory() as temporary:
            artifacts = Path(temporary)
            marker = artifacts / 'bootstrap-result.json'
            marker.write_text('existing evidence')
            result = subprocess.run(['bash', str(Path(host.__file__).with_name('run.sh')),
                                     'hyprland', str(artifacts)], capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(marker.read_text(), 'existing evidence')

    def test_wrapper_mounts_external_evidence_and_cleans_labelled_resources(self):
        fake_docker = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['FAKE_DOCKER_ROOT'])
args = sys.argv[1:]
with (root / 'calls').open('a') as log:
    log.write(json.dumps(args) + '\\n')
if args[:2] == ['container', 'inspect']:
    sys.exit(0 if (root / 'running').exists() else 1)
if args[0] == 'run':
    (root / 'running').touch()
    sys.exit(int(os.environ['FAKE_RUN_CODE']))
if args[0] == 'rm' and args[-1].startswith('open-mpv-vm-'):
    (root / 'running').unlink()
if args[0] == 'ps':
    print('guest-container')
if args[:2] == ['image', 'ls']:
    print('guest-image\\nrunner-image')
if args[:2] == ['image', 'inspect']:
    print('{}')
'''
        for code in (0, 23):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                docker = root / 'docker'
                docker.write_text(fake_docker)
                docker.chmod(0o755)
                artifacts = root / 'outside-checkout'
                result = subprocess.run(['bash', str(Path(host.__file__).with_name('run.sh')),
                                         'gnome', str(artifacts), '--probe'], capture_output=True, timeout=5,
                                        env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}',
                                             'FAKE_DOCKER_ROOT': str(root), 'FAKE_RUN_CODE': str(code)})
                self.assertEqual(result.returncode, code, result.stderr.decode())
                calls = [json.loads(line) for line in (root / 'calls').read_text().splitlines()]
                run = next(call for call in calls if call[0] == 'run')
                self.assertIn(f'type=bind,src={artifacts},dst={artifacts}', run)
                self.assertEqual(run[run.index('--device') + 1], '/dev/kvm')
                self.assertNotIn('--privileged', run)
                self.assertLess(next(i for i, c in enumerate(calls) if c[0] == 'stop'),
                                next(i for i, c in enumerate(calls) if c[0] == 'rm'))
                self.assertIn(['rm', '-f', 'guest-container'], calls)
                self.assertIn(['image', 'rm', 'guest-image'], calls)
                self.assertIn(['image', 'rm', 'runner-image'], calls)
                status = json.loads((artifacts / 'bootstrap-result.json').read_text())
                self.assertEqual(status['status'], 'passed' if code == 0 else 'failed')

    def test_readiness_returns_evidence(self):
        attempts = iter([False, {'ready': True}])
        self.assertEqual(host.wait_for(lambda: next(attempts), 'ready', 1), {'ready': True})

    def test_readiness_fails_on_timeout_and_process_exit(self):
        with self.assertRaisesRegex(TimeoutError, 'timed out.*socket'):
            host.wait_for(lambda: False, 'socket', 0.01)
        with self.assertRaisesRegex(RuntimeError, 'process exited'):
            host.wait_for(lambda: True, 'socket', alive=lambda: False)

    def test_atomic_results_replace_complete_document(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'result.json'
            host.atomic_json(path, {'sequence': 1})
            host.atomic_json(path, {'sequence': 2, 'status': 'failed'})
            self.assertEqual(json.loads(path.read_text()), {'sequence': 2, 'status': 'failed'})
            self.assertFalse(path.with_suffix('.tmp').exists())

    def test_cleanup_kills_descendants_after_leader_exits(self):
        processes = host.Processes()
        with tempfile.TemporaryDirectory() as temporary:
            pid_file = Path(temporary) / 'child.pid'
            code = ('import subprocess, pathlib; '
                    'p = subprocess.Popen(["sleep", "60"]); '
                    f'pathlib.Path({str(pid_file)!r}).write_text(str(p.pid))')
            leader = processes.start([sys.executable, '-c', code], subprocess.DEVNULL)
            try:
                host.wait_for(pid_file.exists, 'child pid', 3)
                child = int(pid_file.read_text())
                leader.wait(timeout=3)
            finally:
                processes.close()

            def gone():
                stat = Path(f'/proc/{child}/stat')
                return not stat.exists() or stat.read_text().split()[2] == 'Z'

            host.wait_for(gone, 'descendant cleanup', 3)

    def test_qmp_handles_events_reply_ids_and_command_errors(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'qmp.sock'
            server = socket.socket(socket.AF_UNIX)
            server.bind(str(path))
            server.listen()
            requests = []
            errors = []

            def serve():
                try:
                    connection, _ = server.accept()
                    connection.settimeout(3)
                    with connection, connection.makefile('rwb') as stream:
                        stream.write(b'{"QMP": {}}\n')
                        stream.flush()
                        for index in range(3):
                            request = json.loads(stream.readline())
                            requests.append(request)
                            stream.write(b'{"event":"RESET"}\n{"id":999,"return":{}}\n')
                            response = {'id': request['id'], 'return': {'ok': True}}
                            if index == 2:
                                response = {'id': request['id'], 'error': {'desc': 'rejected'}}
                            stream.write(json.dumps(response).encode() + b'\n')
                            stream.flush()
                except Exception as error:
                    errors.append(error)

            thread = threading.Thread(target=serve, daemon=True)
            thread.start()
            qmp = host.QMP(path)
            try:
                self.assertEqual(qmp.call('query-version'), {'ok': True})
                with self.assertRaisesRegex(RuntimeError, 'rejected'):
                    qmp.call('bad-command')
            finally:
                qmp.close()
                thread.join(timeout=5)
                server.close()
            self.assertFalse(thread.is_alive())
            self.assertFalse(errors, errors)
            self.assertEqual([r['execute'] for r in requests],
                             ['qmp_capabilities', 'query-version', 'bad-command'])

    def test_input_goes_only_through_qmp(self):
        qmp = Mock()
        events = [{'type': 'key', 'data': {'down': True, 'key': {'type': 'qcode', 'data': 'right'}}}]
        host.service(qmp, {'kind': 'input', 'events': events})
        qmp.call.assert_called_once_with('input-send-event', {'events': events})
        with self.assertRaises(ValueError):
            host.service(qmp, {'kind': 'dispatch-action'})

    def test_host_rejects_guest_file_write_requests(self):
        qmp = Mock()
        with self.assertRaises(ValueError):
            host.service(qmp, {'kind': 'screenshot', 'name': '../escape.png'})
        qmp.call.assert_not_called()

    def test_build_failure_removes_image_and_preserves_failure(self):
        args = argparse.Namespace(desktop='gnome', probe=True, base=None, image=None)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with patch.object(host.subprocess, 'run') as run:
                run.side_effect = [subprocess.CalledProcessError(1, 'docker build'), None]
                with self.assertRaises(subprocess.CalledProcessError):
                    host.prepare(args, directory, directory)
            self.assertEqual(run.call_args_list[-1].args[0][:3], ['docker', 'image', 'rm'])

    def test_result_cannot_omit_skip_or_relabel_required_scenarios(self):
        with tempfile.TemporaryDirectory() as temporary:
            artifacts = Path(temporary)
            (artifacts / 'probe-only').touch()
            probe = {'name': 'compositor-routed-input-probe', 'status': 'passed'}
            result = {'status': 'passed', 'probe_only': True, 'tests': [probe]}
            host.validate_result(result, artifacts)
            for bad in ({**result, 'tests': []}, {**result, 'probe_only': False},
                        {**result, 'tests': [{**probe, 'status': 'skipped'}]}):
                with self.subTest(bad=bad), self.assertRaises(RuntimeError):
                    host.validate_result(bad, artifacts)

            (artifacts / 'probe-only').unlink()
            (artifacts / 'ignored-inventory.txt').write_text('window::regression: test\n')
            result['probe_only'] = False
            with self.assertRaisesRegex(RuntimeError, 'omitted'):
                host.validate_result(result, artifacts)
            result['tests'] += [{'name': name, 'status': 'passed'} for name in
                                ('shared-viewer-and-desktop-behavior', 'rust-default', 'window-regression')]
            host.validate_result(result, artifacts)
            (artifacts / 'ignored-inventory.txt').write_text('0 tests\n')
            with self.assertRaisesRegex(RuntimeError, 'empty'):
                host.validate_result(result, artifacts)

    def test_guest_failure_and_no_result_are_not_success(self):
        # Exercise the real VM supervision loop with only external processes replaced.
        for result in ({'status': 'failed'}, None):
            with self.subTest(result=result), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                artifacts = directory / 'artifacts'
                artifacts.mkdir()
                (artifacts / 'session-started').touch()
                (directory / 'qmp.sock').touch()
                if result:
                    host.atomic_json(artifacts / 'result.json', result)

                def start(command, _log, **_kwargs):
                    if command[0] == 'Xvfb':
                        (directory / 'display').write_text('99\n')
                    else:
                        self.assertEqual(_kwargs['env']['SDL_VIDEODRIVER'], 'x11')
                        self.assertNotIn('WAYLAND_DISPLAY', _kwargs['env'])
                        self.assertNotIn('DBUS_SESSION_BUS_ADDRESS', _kwargs['env'])
                    return Mock(poll=lambda: None)

                with patch.dict(os.environ, WAYLAND_DISPLAY='host-wayland', DBUS_SESSION_BUS_ADDRESS='host-bus'), \
                     patch.object(host.Processes, 'start', side_effect=start), \
                     patch.object(host.Processes, 'close') as cleanup, \
                     patch.object(host, 'QMP') as qmp:
                    qmp.return_value.call.return_value = {}
                    with self.assertRaises((RuntimeError, TimeoutError)):
                        host.run_vm(directory, artifacts, timeout=0.01 if result is None else 1)
                    cleanup.assert_called_once()


class FixtureTests(unittest.TestCase):
    def test_png_dimensions_filters_and_rgb(self):
        image = fixtures.png(2, 3, bytes((200, 30, 40)) * 6)
        self.assertEqual(image[:8], b'\x89PNG\r\n\x1a\n')
        self.assertEqual(struct.unpack('!II', image[16:24]), (2, 3))
        data_size = struct.unpack('!I', image[33:37])[0]
        self.assertEqual(zlib.decompress(image[41:41 + data_size]),
                         (b'\0' + bytes((200, 30, 40)) * 2) * 3)

    def test_fixture_sets_hashes_and_repeatability(self):
        def video(command, **kwargs):
            self.assertEqual(kwargs, {'check': True, 'timeout': 60})
            self.assertIn('+bitexact', command)
            Path(command[-1]).write_bytes(b'fake-video-for-unit-test')

        with tempfile.TemporaryDirectory() as temporary, patch.object(fixtures.subprocess, 'run', side_effect=video):
            first, second = (Path(temporary) / name for name in ('first', 'second'))
            fixtures.generate(first)
            fixtures.generate(second)
            hashes = json.loads((first / 'sha256.json').read_text())
            self.assertEqual(len(hashes), 17)
            self.assertEqual(len(list((first / 'images').glob('*.png'))), 3)
            self.assertEqual(len(list((first / 'stress').glob('*.png'))), 12)
            self.assertEqual((first / 'sha256.json').read_bytes(), (second / 'sha256.json').read_bytes())
            self.assertIn('broken/broken.png', hashes)


if __name__ == '__main__':
    unittest.main()
