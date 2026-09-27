#!/usr/bin/env python3
"""Check video idle inhibition against a live Hyprland session."""

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def hyprland(command):
    return json.loads(subprocess.check_output(["hyprctl", "-j", command]))


def window(pid):
    matches = [client for client in hyprland("clients") if client.get("pid") == pid]
    if len(matches) > 1:
        raise RuntimeError(f"expected one open-mpv window, found {len(matches)}")
    return matches[0] if matches else None


def wait_for_inhibition(pid, expected, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        client = window(pid)
        if client is not None and client["inhibitingIdle"] is expected:
            return
        time.sleep(0.1)
    raise RuntimeError(f"Hyprland did not report inhibitingIdle={expected}")


def require_focus(pid):
    if hyprland("activewindow").get("pid") != pid:
        client = window(pid)
        if client is None:
            raise RuntimeError("test window disappeared before it could be focused")
        dispatcher = f'hl.dsp.focus({{ window = "address:{client["address"]}" }})'
        subprocess.run(["hyprctl", "dispatch", dispatcher],
                       check=True, capture_output=True)
    if hyprland("activewindow").get("pid") != pid:
        raise RuntimeError("focus the test window and run the check again")


def make_video(path, seconds):
    subprocess.run([
        "ffmpeg", "-loglevel", "error", "-f", "lavfi", "-i",
        "color=c=black:s=320x240:r=10", "-t", str(seconds),
        "-c:v", "libx264", "-pix_fmt", "yuv420p", str(path),
    ], check=True)


def main(binary):
    if any(client.get("class") == "io.github.TheRealShek.OpenMpv"
           for client in hyprland("clients")):
        raise RuntimeError("close the existing open-mpv window before this check")

    with tempfile.TemporaryDirectory(prefix="open-mpv-idle-") as directory:
        config_dir = Path(directory) / "config"
        config_file = config_dir / "open-mpv" / "open-mpv.conf"
        config_file.parent.mkdir(parents=True)
        config_file.write_text("loop=no\n")
        app_env = {**os.environ, "XDG_CONFIG_HOME": str(config_dir)}
        video = Path(directory) / "video.mp4"
        make_video(video, 30)

        with (Path(directory) / "open-mpv.log").open("w+") as log:
            app = subprocess.Popen([str(binary.resolve()), str(video)],
                                   stdout=log, stderr=log, env=app_env)
            try:
                wait_for_inhibition(app.pid, True)
                require_focus(app.pid)

                subprocess.run(["wtype", "-k", "space"], check=True)
                wait_for_inhibition(app.pid, False)
                subprocess.run(["wtype", "-k", "space"], check=True)
                wait_for_inhibition(app.pid, True)
                subprocess.run(["wtype", "-k", "Escape"], check=True)
                app.wait(timeout=5)
                if app.returncode != 0:
                    raise RuntimeError(f"open-mpv exited with status {app.returncode}")
                if any(client.get("pid") == app.pid for client in hyprland("clients")):
                    raise RuntimeError("open-mpv window remains after exit")
                print("Idle inhibition follows play, pause, resume and exit")
            except Exception:
                log.seek(0)
                for line in log:
                    if "idle inhibit" in line or "player:" in line or "error" in line.lower():
                        print(line.rstrip(), file=sys.stderr)
                raise
            finally:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=5)

        short_video = Path(directory) / "finished.mp4"
        make_video(short_video, 12)
        eos_log = Path(directory) / "eos.log"
        with eos_log.open("w+") as log:
            app = subprocess.Popen([str(binary.resolve()), str(short_video)],
                                   stdout=log, stderr=log, env=app_env)
            try:
                wait_for_inhibition(app.pid, True)
                wait_for_inhibition(app.pid, False, timeout=18)
                require_focus(app.pid)
                subprocess.run(["wtype", "-M", "shift", "-k", "Left", "-m", "shift"],
                               check=True)
                time.sleep(0.5)
                wait_for_inhibition(app.pid, False)
                log.flush()
                if "player: seek to" not in eos_log.read_text():
                    raise RuntimeError("seek after natural end did not reach open-mpv")
                require_focus(app.pid)
                subprocess.run(["wtype", "-k", "space"], check=True)
                wait_for_inhibition(app.pid, True)
                log.flush()
                if "player: seek to 0.0s" in eos_log.read_text():
                    raise RuntimeError("playback rewound after an end-of-stream seek")
                print("Idle inhibition releases at video end and after an end-of-stream seek")
            except Exception:
                for line in eos_log.read_text().splitlines():
                    if "idle inhibit" in line or "player:" in line or "error" in line.lower():
                        print(line.rstrip(), file=sys.stderr)
                raise
            finally:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=5)

        config_file.write_text("loop=yes\n")
        looping_video = Path(directory) / "looping.mp4"
        make_video(looping_video, 2)
        with (Path(directory) / "loop.log").open("w+") as log:
            app = subprocess.Popen([str(binary.resolve()), str(looping_video)],
                                   stdout=log, stderr=log, env=app_env)
            try:
                wait_for_inhibition(app.pid, True)
                time.sleep(3)
                wait_for_inhibition(app.pid, True)
                print("Idle inhibition persists when video loops")
            except Exception:
                log.seek(0)
                for line in log:
                    if "idle inhibit" in line or "error" in line.lower():
                        print(line.rstrip(), file=sys.stderr)
                raise
            finally:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: check-idle.py /path/to/open-mpv")
    main(Path(sys.argv[1]))
