#!/usr/bin/env python3
"""Boot an isolated desktop VM and bridge its requests to QEMU input devices."""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import traceback
from pathlib import Path

from fixtures import png


def wait_for(predicate, description, timeout=30, alive=lambda: True):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not alive():
            raise RuntimeError(f'process exited while waiting for {description}')
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise TimeoutError(f'timed out waiting for {description}')


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


class Processes:
    def __init__(self):
        self.children = []

    def start(self, command, log, **kwargs):
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True, **kwargs)
        self.children.append(process)
        return process

    def close(self):
        # Kill the whole session even if its leader exited before its children.
        for process in reversed(self.children):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 5
        for process in reversed(self.children):
            try:
                process.wait(timeout=max(0.01, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)

    def stop(self, process):
        # Also reap descendants from a command that completed normally.
        pending = self.children
        self.children = [process]
        try:
            self.close()
        finally:
            self.children = [child for child in pending if child is not process]


class QMP:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(10)
        self.socket.connect(str(path))
        self.stream = self.socket.makefile('rwb')
        greeting = json.loads(self.stream.readline())
        if 'QMP' not in greeting:
            raise RuntimeError(f'bad QMP greeting: {greeting}')
        self.sequence = 0
        self.call('qmp_capabilities')

    def call(self, command, arguments=None):
        self.sequence += 1
        request = {'execute': command, 'id': self.sequence}
        if arguments is not None:
            request['arguments'] = arguments
        self.stream.write(json.dumps(request).encode() + b'\n')
        self.stream.flush()
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError('QMP disconnected')
            response = json.loads(line)
            if response.get('id') != self.sequence:
                continue
            if 'error' in response:
                raise RuntimeError(f'QMP {command}: {response["error"]}')
            return response['return']

    def close(self):
        self.stream.close()
        self.socket.close()


def screenshot(qmp, output):
    ppm = output.with_suffix('.ppm')
    qmp.call('screendump', {'filename': str(ppm)})
    with ppm.open('rb') as image:
        if image.readline() != b'P6\n':
            raise RuntimeError('unexpected screendump format')
        width, height = map(int, image.readline().split())
        if image.readline() != b'255\n':
            raise RuntimeError('unexpected screendump depth')
        pixels = image.read()
    if len(pixels) != width * height * 3:
        raise RuntimeError('incomplete screendump')
    output.write_bytes(png(width, height, pixels))
    ppm.unlink()


def service(qmp, request, artifacts):
    if request['kind'] == 'input':
        # No app actions or desktop IPC dispatchers are accepted here.
        qmp.call('input-send-event', {'events': request['events']})
    elif request['kind'] == 'screenshot':
        name = request['name']
        if Path(name).name != name or not name.endswith('.png'):
            raise ValueError('screenshot must be a PNG basename')
        screenshot(qmp, artifacts / name)
    else:
        raise ValueError(f'unknown request: {request["kind"]}')


def validate_result(value, artifacts):
    probe_only = (artifacts / 'probe-only').exists()
    tests = value.get('tests', [])
    if (value.get('status') != 'passed' or value.get('probe_only') is not probe_only
            or not tests or any(test.get('status') != 'passed' for test in tests)):
        raise RuntimeError(f'guest suite failed or returned incomplete results: {value}')
    required = {'compositor-routed-input-probe'}
    if not probe_only:
        required.update(('shared-viewer-and-desktop-behavior', 'rust-default'))
        inventory = (artifacts / 'ignored-inventory.txt').read_text()
        ignored = {line.removesuffix(': test').replace('::', '-')
                   for line in inventory.splitlines() if line.endswith(': test')}
        if not ignored:
            raise RuntimeError('guest returned an empty desktop regression inventory')
        required.update(ignored)
    if not required.issubset({test['name'] for test in tests}):
        raise RuntimeError(f'guest result omitted required scenarios: {required}')


def prepare(args, directory, artifacts):
    tag = args.image or f'open-mpv-ci-{args.desktop}:{os.getpid()}'
    label = 'io.github.therealshek.openmpv.ci-run=' + os.environ.get('OPEN_MPV_CI_RUN_ID', f'host-{os.getpid()}')
    container = None
    try:
        if not args.image:
            with (artifacts / 'build.log').open('w') as log:
                subprocess.run([
                    'docker', 'build', '--progress=plain', '-f', 'ci/desktop/Dockerfile',
                    '--build-arg', f'BASE={args.base or ("fedora:44" if args.desktop == "gnome" else "archlinux:base")}',
                    '--build-arg', f'DESKTOP={args.desktop}', '--build-arg',
                    f'PROBE_ONLY={int(args.probe)}', '--label', label, '-t', tag, '.',
                ], check=True, timeout=2400, stdout=log, stderr=subprocess.STDOUT)
        image_info = subprocess.check_output(['docker', 'image', 'inspect', tag], text=True, timeout=30)
        (artifacts / 'guest-image.json').write_text(image_info)
        container = subprocess.check_output(['docker', 'create', '--label', label, tag, '/bin/true'], text=True).strip()
        root = directory / 'root'
        root.mkdir()
        archive = directory / 'root.tar'
        subprocess.run(['docker', 'export', '-o', str(archive), container], check=True, timeout=120)
        subprocess.run(['tar', '-xf', str(archive), '--exclude=dev/*', '-C', str(root)],
                       check=True, timeout=120)
        archive.unlink()
        disk = directory / 'guest.ext4'
        with disk.open('wb') as file:
            file.truncate(12 * 1024**3)
        subprocess.run(['mkfs.ext4', '-q', '-F', '-d', str(root), str(disk)], check=True, timeout=180)
        for name in ('vmlinuz-ci', 'initramfs-ci.img'):
            shutil.copy2(root / 'boot' / name, directory / name)
        shutil.rmtree(root)
    finally:
        if container:
            subprocess.run(['docker', 'rm', '-f', container], check=True, timeout=30)
        if not args.image:
            subprocess.run(['docker', 'image', 'rm', tag], check=False, timeout=30,
                           stdout=subprocess.DEVNULL)


def run_vm(directory, artifacts, timeout):
    processes = Processes()
    qmp = None
    try:
        with (artifacts / 'xvfb.log').open('w') as log:
            display_file = directory / 'display'
            with display_file.open('w') as display:
                xvfb = processes.start(['Xvfb', '-displayfd', str(display.fileno()), '-screen',
                                        '0', '1280x800x24', '-nolisten', 'tcp'],
                                       log, pass_fds=(display.fileno(),))
            number = wait_for(lambda: display_file.read_text().strip(), 'Xvfb',
                              alive=lambda: xvfb.poll() is None)
        environment = {key: value for key, value in os.environ.items()
                       if key not in ('WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS')}
        environment.update(DISPLAY=f':{number}', LIBGL_ALWAYS_SOFTWARE='1', SDL_VIDEODRIVER='x11')
        qmp_path = directory / 'qmp.sock'
        command = [
            'qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host', '-smp', '2', '-m', '4096',
            '-kernel', str(directory / 'vmlinuz-ci'), '-initrd', str(directory / 'initramfs-ci.img'),
            '-append', 'root=/dev/vda rw console=tty0 console=ttyS0 quiet',
            '-drive', f'file={directory / "guest.ext4"},format=raw,if=virtio',
            '-device', 'virtio-vga-gl', '-display', 'sdl,gl=on',
            '-device', 'virtio-keyboard-pci', '-device', 'virtio-tablet-pci',
            '-nic', 'none', '-no-reboot', '-monitor', 'none',
            '-serial', f'file:{artifacts / "serial.log"}',
            '-qmp', f'unix:{qmp_path},server=on,wait=off',
            '-virtfs', f'local,path={artifacts},mount_tag=artifacts,security_model=none',
        ]
        atomic_json(artifacts / 'qemu-command.json', command)
        with (artifacts / 'qemu.log').open('w') as log:
            vm = processes.start(command, log, env=environment)
        alive = lambda: vm.poll() is None and xvfb.poll() is None
        wait_for(qmp_path.exists, 'QMP socket', alive=alive)
        qmp = QMP(qmp_path)
        atomic_json(artifacts / 'qemu-version.json', qmp.call('query-version'))
        wait_for(lambda: (artifacts / 'session-started').exists(), 'guest login', 180, alive)
        deadline = time.monotonic() + timeout
        sequence = 0
        while time.monotonic() < deadline:
            result = artifacts / 'result.json'
            if result.exists():
                value = json.loads(result.read_text())
                validate_result(value, artifacts)
                return
            if not alive():
                raise RuntimeError('VM or Xvfb exited before a result')
            request_file = artifacts / 'request.json'
            if request_file.exists():
                request = json.loads(request_file.read_text())
                if request['sequence'] > sequence:
                    sequence = request['sequence']
                    service(qmp, request, artifacts)
                    atomic_json(artifacts / 'response.json', {'sequence': sequence})
            time.sleep(0.05)
        raise TimeoutError('guest suite exceeded its wall-clock limit')
    finally:
        if qmp:
            try:
                screenshot(qmp, artifacts / 'final.png')
            except (OSError, RuntimeError):
                traceback.print_exc()
            qmp.close()
        processes.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('desktop', choices=('gnome', 'hyprland'))
    parser.add_argument('--artifacts', required=True, type=Path)
    parser.add_argument('--probe', action='store_true', help='prove VM/compositor/input before compiling Rust')
    parser.add_argument('--base', help='local diagnostic base image; CI uses the official distribution image')
    parser.add_argument('--image', help='reuse an already built guest image for local harness diagnosis')
    args = parser.parse_args()
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt('runner interrupted')

    signal.signal(signal.SIGTERM, interrupted)
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=False)
    artifacts.chmod(0o777)  # Dedicated, disposable 9p share, writable by guest uid 1000.
    status = 'failed'
    try:
        atomic_json(artifacts / 'host-metadata.json', {
            'desktop': args.desktop, 'probe': args.probe,
            'kernel': os.uname().release,
            'base': args.base or ('fedora:44' if args.desktop == 'gnome' else 'archlinux:base'),
            'commit': subprocess.check_output(['git', '-c', f'safe.directory={Path.cwd()}',
                                               'rev-parse', 'HEAD'], text=True).strip(),
        })
        if Path('/runner-packages.txt').exists():
            shutil.copy2('/runner-packages.txt', artifacts / 'runner-packages.txt')
        if args.probe:
            (artifacts / 'probe-only').touch()
        with tempfile.TemporaryDirectory(prefix='open-mpv-vm-') as temporary:
            directory = Path(temporary)
            prepare(args, directory, artifacts)
            run_vm(directory, artifacts, timeout=1800)
        status = 'passed'
    finally:
        atomic_json(artifacts / 'host-result.json', {'status': status})
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
