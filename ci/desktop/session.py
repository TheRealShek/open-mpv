#!/usr/bin/env python3
"""Shared scenarios in a guest login. IPC observes state; QEMU supplies input."""

import json
import os
import shutil
import subprocess
import sys
import time
import traceback
from pathlib import Path

import gi
from PIL import Image

gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib

from fixtures import generate
from host import Processes, atomic_json, wait_for

ARTIFACTS = Path('/artifacts')
ROOT = Path('/opt/open-mpv')


class Input:
    def __init__(self, desktop):
        self.sequence = 0
        self.desktop = desktop

    def request(self, **request):
        self.sequence += 1
        atomic_json(ARTIFACTS / 'request.json', {'sequence': self.sequence, **request})

        def acknowledged():
            response = ARTIFACTS / 'response.json'
            return response.exists() and json.loads(response.read_text())['sequence'] == self.sequence

        wait_for(acknowledged, 'host input acknowledgement', 15)

    def events(self, events):
        self.request(kind='input', events=events)

    def key(self, *keys):
        def event(key, down):
            return {'type': 'key', 'data': {'down': down, 'key': {'type': 'qcode', 'data': key}}}

        self.events([event(key, True) for key in keys])
        time.sleep(0.08)
        self.events([event(key, False) for key in reversed(keys)])

    def double_click(self, x, y, width, height):
        self.events([{'type': 'abs', 'data': {'axis': axis, 'value': round(value * 32767 / size)}}
                     for axis, value, size in (('x', x, width), ('y', y, height))])
        for _ in range(2):
            self.events([{'type': 'btn', 'data': {'button': 'left', 'down': True}}])
            time.sleep(0.04)
            self.events([{'type': 'btn', 'data': {'button': 'left', 'down': False}}])

    def capture(self, name):
        path = ARTIFACTS / f'{name}.png'
        self.desktop.capture(path)
        return path


class Desktop:
    def __init__(self, name):
        self.name = name
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def evaluate(self, script):
        result = self.bus.call_sync(
            'org.gnome.Shell', '/org/gnome/Shell', 'org.gnome.Shell', 'Eval',
            GLib.Variant('(s)', (f'JSON.stringify({script})',)),
            None, Gio.DBusCallFlags.NONE, 3000, None).unpack()
        if not result[0]:
            raise RuntimeError(f'GNOME Shell observation failed: {result[1]}')
        return json.loads(result[1])

    def hypr(self, command):
        return json.loads(subprocess.check_output(['hyprctl', '-j', command], timeout=5))

    def capture(self, path):
        path.unlink(missing_ok=True)
        if self.name == 'gnome':
            result = self.bus.call_sync(
                'org.gnome.Shell.Screenshot', '/org/gnome/Shell/Screenshot',
                'org.gnome.Shell.Screenshot', 'Screenshot',
                GLib.Variant('(bbs)', (False, False, str(path))), None,
                Gio.DBusCallFlags.NONE, 10000, None).unpack()
            if not result[0]:
                raise RuntimeError(f'GNOME Shell screenshot failed: {result}')
        else:
            subprocess.run(['grim', str(path)], check=True, timeout=10)

    def windows(self):
        if self.name == 'gnome':
            return self.evaluate('global.get_window_actors().map(a => { const w = a.meta_window; '
                                 'const r = w.get_frame_rect(); return {pid:w.get_pid(), '
                                 'title:w.get_title(), fullscreen:w.is_fullscreen(), '
                                 'focused:w.has_focus(), x:r.x, y:r.y, width:r.width, height:r.height}; })')
        return [{'pid': w['pid'], 'title': w['title'], 'fullscreen': bool(w['fullscreen']),
                 'focused': w['address'] == self.hypr('activewindow').get('address'),
                 'x': w['at'][0], 'y': w['at'][1], 'width': w['size'][0], 'height': w['size'][1],
                 'floating': w['floating'], 'inhibitingIdle': w['inhibitingIdle']}
                for w in self.hypr('clients')]

    def window(self, pid):
        windows = [window for window in self.windows() if window['pid'] == pid]
        if len(windows) > 1:
            raise RuntimeError(f'multiple windows for pid {pid}: {windows}')
        return windows[0] if windows else None


class Suite:
    def __init__(self, desktop):
        self.desktop = Desktop(desktop)
        self.input = Input(self.desktop)
        self.processes = Processes()
        self.results = []
        self.logs = []

    def start(self, command, name):
        log = (ARTIFACTS / f'{name}.log').open('w')
        self.logs.append(log)
        return self.processes.start(command, log)

    def case(self, name, operation):
        started = time.monotonic()
        result = {'name': name, 'status': 'failed'}
        try:
            operation()
            result['status'] = 'passed'
        except Exception:
            result['error'] = traceback.format_exc()
            print(result['error'], flush=True)
            try:
                self.input.capture(f'failure-{name}')
            except Exception:
                traceback.print_exc()
        result['seconds'] = round(time.monotonic() - started, 3)
        self.results.append(result)
        atomic_json(ARTIFACTS / 'progress.json', self.results)
        return result['status'] == 'passed'

    def window(self, process, **expected):
        def matches():
            window = self.desktop.window(process.pid)
            return window if window and all(window[k] == v for k, v in expected.items()) else None

        return wait_for(matches, f'window {expected}', 20, lambda: process.poll() is None)

    def color(self, name, window, channel):
        def displayed():
            path = self.input.capture(name)
            with Image.open(path) as image:
                x = window['x'] + window['width'] // 2
                y = window['y'] + window['height'] // 2
                r, g, b = image.convert('RGB').getpixel((x, y))
            values = (r, g, b)
            return values[channel] > 170 and all(v < 90 for i, v in enumerate(values) if i != channel)

        wait_for(displayed, f'displayed {name} pixels', 20)

    def pointer_fullscreen(self, process, name):
        window = self.window(process, focused=True, fullscreen=False)
        with Image.open(self.input.capture(f'{name}-before-pointer')) as image:
            width, height = image.size
        self.input.double_click(window['x'] + window['width'] // 2,
                                window['y'] + window['height'] // 2, width, height)
        window = self.window(process, fullscreen=True)
        self.color(f'{name}-pointer-fullscreen', window, 1)
        self.input.key('esc')
        self.window(process, fullscreen=False, focused=True)

    def probe(self):
        process = self.start(['python3', 'ci/desktop/input-probe.py'], 'input-probe')
        try:
            window = self.window(process, focused=True, fullscreen=False)
            self.color('probe-before-input', window, 0)
            self.input.key('right')
            self.color('probe-keyboard', window, 1)
            self.pointer_fullscreen(process, 'probe')
        finally:
            self.processes.stop(process)

    def rust(self, name, arguments, timeout):
        process = self.start([str(ROOT / 'bin/tests'), *arguments], name)
        try:
            code = process.wait(timeout=timeout)
        finally:
            self.processes.stop(process)
        if code:
            raise RuntimeError(f'{name} failed ({code}); see {name}.log')
        if '--exact' in arguments:
            output = (ARTIFACTS / f'{name}.log').read_text()
            if 'test result: ok. 1 passed;' not in output:
                raise RuntimeError(f'{name} did not execute exactly one passing test')

    def behavior(self, fixtures):
        process = self.start([str(ROOT / 'bin/open-mpv'), str(fixtures / 'images/01.png')], 'viewer')
        try:
            window = self.window(process, focused=True, fullscreen=False)
            self.color('viewer-open', window, 0)
            self.input.key('right')
            window = self.window(process, focused=True)
            wait_for(lambda: '02.png' in self.desktop.window(process.pid)['title'], 'next title')
            self.color('viewer-next', window, 1)
            self.pointer_fullscreen(process, 'viewer')

            path = fixtures / 'images/02.png'
            original = path.read_bytes()
            self.input.key('delete')
            wait_for(lambda: not path.exists(), 'keyboard Trash')
            trash = Path(os.environ['XDG_DATA_HOME']) / 'Trash'
            wait_for(lambda: any(trash.glob('files/*')), 'isolated trash contents')
            self.input.capture('viewer-trashed')
            self.input.key('ctrl', 'z')
            wait_for(path.exists, 'keyboard Undo')
            if path.read_bytes() != original:
                raise RuntimeError('Undo changed source bytes')
            self.input.capture('viewer-restored')

            # A later launch must exit and reuse the existing window over this bus.
            subprocess.run([str(ROOT / 'bin/open-mpv'), str(fixtures / 'images/01.png')],
                           check=True, timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: '01.png' in self.desktop.window(process.pid)['title'], 'forwarded open')
            self.color('viewer-forwarded-open', self.window(process, focused=True), 0)
            if len([w for w in self.desktop.windows() if w['pid'] == process.pid]) != 1:
                raise RuntimeError('single-instance open created extra windows')

            self.desktop_checks(process, fixtures)
        finally:
            self.processes.stop(process)

    def desktop_checks(self, process, fixtures):
        if self.desktop.name == 'gnome':
            self.input.key('meta_l')
            wait_for(lambda: self.desktop.evaluate('Main.overview.visible'), 'GNOME overview')
            self.input.capture('gnome-overview')
            self.input.key('esc')
            wait_for(lambda: not self.desktop.evaluate('Main.overview.visible'), 'GNOME overview dismissed')
            self.window(process, focused=True)
            self.input.capture('gnome-focus-returned')
        else:
            before = self.window(process)['floating']
            self.input.key('meta_l', 'v')
            wait_for(lambda: self.desktop.window(process.pid)['floating'] != before,
                     'Hyprland compositor binding')
            self.input.capture('hyprland-binding')
            self.input.key('meta_l', 'v')
            wait_for(lambda: self.desktop.window(process.pid)['floating'] == before,
                     'Hyprland binding restored')
            subprocess.run([str(ROOT / 'bin/open-mpv'), str(fixtures / 'video/test.webm')],
                           check=True, timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: self.desktop.window(process.pid)['inhibitingIdle'], 'Hyprland video idle inhibitor')
            self.input.key('spc')
            wait_for(lambda: not self.desktop.window(process.pid)['inhibitingIdle'], 'paused idle inhibitor release')
            self.input.capture('hyprland-paused')
            self.input.key('spc')
            wait_for(lambda: self.desktop.window(process.pid)['inhibitingIdle'], 'resumed idle inhibitor')

    def close(self):
        self.processes.close()
        for log in self.logs:
            log.close()


def main():
    name = sys.argv[1]
    (ARTIFACTS / 'session-started').touch()
    suite = Suite(name)
    try:
        shutil.copy2('/opt/open-mpv/packages.txt', ARTIFACTS / 'packages.txt')
        shutil.copy2('/etc/os-release', ARTIFACTS / 'os-release')
        atomic_json(ARTIFACTS / 'guest-metadata.json', {
            'desktop': name, 'kernel': os.uname().release,
            'environment': {key: os.environ.get(key) for key in
                            ('HOME', 'XDG_RUNTIME_DIR', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME',
                             'XDG_SESSION_TYPE', 'DBUS_SESSION_BUS_ADDRESS')},
        })
        runtime = os.environ.get('XDG_RUNTIME_DIR')
        if not runtime:
            raise RuntimeError('login did not establish XDG_RUNTIME_DIR; check PAM and logind')
        runtime_stat = Path(runtime).stat()
        if runtime_stat.st_uid != os.getuid() or runtime_stat.st_mode & 0o777 != 0o700:
            raise RuntimeError('login runtime directory must be private and owned by the test user')
        with (ARTIFACTS / 'login-session.txt').open('w') as log:
            subprocess.run(['loginctl', 'show-session', 'self'], check=True, timeout=5,
                           stdout=log, stderr=subprocess.STDOUT)
        if name == 'gnome':
            os.environ['XDG_CURRENT_DESKTOP'] = 'GNOME'
            subprocess.run(['gsettings', 'set', 'org.gnome.desktop.interface', 'enable-animations', 'false'],
                           check=True, timeout=5)
            compositor = suite.start(['gnome-shell', '--wayland', '--unsafe-mode'], 'compositor')
        else:
            os.environ['XDG_CURRENT_DESKTOP'] = 'Hyprland'
            config = Path(os.environ['XDG_CONFIG_HOME']) / 'hyprland.lua'
            config.write_text('''hl.monitor({ output = "", mode = "1280x800@60", position = "auto", scale = 1 })
hl.config({ animations = { enabled = false }, misc = { disable_hyprland_logo = true }, debug = { enable_stdout_logs = true } })
hl.permission("/usr/bin/grim", "screencopy", "allow")
hl.bind("SUPER + V", hl.dsp.window.float({ action = "toggle" }))
''')
            compositor = suite.start(['Hyprland', '--config', str(config)], 'compositor')

        def ready():
            sockets = list(Path(os.environ['XDG_RUNTIME_DIR']).glob('wayland-*'))
            sockets = [path for path in sockets if path.is_socket()]
            if len(sockets) != 1:
                return False
            os.environ['WAYLAND_DISPLAY'] = sockets[0].name
            if name == 'hyprland':
                instances = list((Path(os.environ['XDG_RUNTIME_DIR']) / 'hypr').glob('*/.socket.sock'))
                if len(instances) != 1:
                    return False
                os.environ['HYPRLAND_INSTANCE_SIGNATURE'] = instances[0].parent.name
            try:
                suite.desktop.windows()
                return True
            except (GLib.Error, subprocess.SubprocessError):
                return False

        wait_for(ready, f'{name} compositor and IPC', 90, lambda: compositor.poll() is None)
        if not suite.case('compositor-routed-input-probe', suite.probe):
            return
        if (ARTIFACTS / 'probe-only').exists():
            return
        fixtures = Path(os.environ['HOME']) / 'fixtures'
        generate(fixtures)
        shutil.copy2(fixtures / 'sha256.json', ARTIFACTS / 'fixture-sha256.json')
        os.environ.update(OPEN_MPV_STRESS_DIR=str(fixtures / 'stress'),
                          OPEN_MPV_TRANSITION_IMAGES=str(fixtures / 'images'),
                          OPEN_MPV_TRANSITION_VIDEO=str(fixtures / 'video/test.webm'))
        suite.case('shared-viewer-and-desktop-behavior', lambda: suite.behavior(fixtures))
        suite.case('rust-default', lambda: suite.rust('rust-default', ['--test-threads=1'], 180))
        listing = subprocess.check_output([str(ROOT / 'bin/tests'), '--list', '--ignored'], text=True, timeout=10)
        (ARTIFACTS / 'ignored-inventory.txt').write_text(listing)
        tests = [line.removesuffix(': test') for line in listing.splitlines() if line.endswith(': test')]
        if not tests:
            raise RuntimeError('desktop regression inventory is empty')
        for test in tests:
            label = test.replace('::', '-')
            suite.case(label, lambda test=test, label=label: suite.rust(label, ['--ignored', '--exact', test], 180))
    except Exception:
        suite.results.append({'name': 'setup', 'status': 'failed', 'error': traceback.format_exc()})
        traceback.print_exc()
    finally:
        if os.environ.get('WAYLAND_DISPLAY'):
            try:
                suite.input.capture('final')
            except Exception:
                traceback.print_exc()
        suite.close()
        atomic_json(ARTIFACTS / 'result.json', {
            'status': 'passed' if suite.results and all(r['status'] == 'passed' for r in suite.results) else 'failed',
            'desktop': name, 'probe_only': (ARTIFACTS / 'probe-only').exists(), 'tests': suite.results,
        })


if __name__ == '__main__':
    main()
