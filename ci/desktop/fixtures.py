"""Small deterministic fixtures, independent of installed ImageMagick defaults."""

import hashlib
import json
import struct
import subprocess
import zlib
from pathlib import Path


def png(width, height, rgb):
    def chunk(kind, data):
        return (struct.pack('!I', len(data)) + kind + data
                + struct.pack('!I', zlib.crc32(kind + data)))

    rows = b''.join(b'\0' + rgb[y * width * 3:(y + 1) * width * 3]
                    for y in range(height))
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('!2I5B', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def generate(directory):
    directory = Path(directory)
    for name in ('images', 'stress', 'video', 'broken'):
        (directory / name).mkdir(parents=True)
    for index, color in enumerate(((220, 30, 30), (30, 220, 30), (30, 30, 220))):
        (directory / 'images' / f'{index + 1:02}.png').write_bytes(
            png(320, 240, bytes(color) * 320 * 240))
    for index in range(12):
        (directory / 'stress' / f'{index:02}.png').write_bytes(
            png(320, 240, bytes((index * 20, 40, 160)) * 320 * 240))
    (directory / 'broken' / 'broken.png').write_bytes(b'not an image\n')
    subprocess.run([
        'ffmpeg', '-nostdin', '-v', 'error', '-f', 'lavfi', '-i',
        'color=c=blue:s=320x240:r=10', '-t', '20', '-an', '-c:v', 'libvpx',
        '-threads', '1', '-fflags', '+bitexact', '-flags:v', '+bitexact',
        '-map_metadata', '-1', str(directory / 'video' / 'test.webm'),
    ], check=True, timeout=60)
    hashes = {str(p.relative_to(directory)): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(directory.rglob('*')) if p.is_file()}
    (directory / 'sha256.json').write_text(json.dumps(hashes, indent=2) + '\n')
