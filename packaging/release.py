#!/usr/bin/env python3
"""Prepare draft assets without replacing published or conflicting bytes."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib

RPM = 'open-mpv-fedora44-x86_64.rpm'
ASSETS = ('release-source.json', RPM, 'SHA256SUMS')


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def api(path):
    return json.loads(run('gh', 'api', f'repos/{os.environ["GH_REPO"]}/{path}'))


def pages(path):
    page = 1
    while True:
        separator = '&' if '?' in path else '?'
        items = api(f'{path}{separator}per_page=100&page={page}')
        yield from items
        if len(items) < 100:
            return
        page += 1


def release(tag):
    return next((item for item in pages('releases') if item['tag_name'] == tag), None)


def require_draft(item):
    if item and (not item['draft'] or item.get('immutable')):
        raise ValueError('Published releases cannot be changed; use a new version.')


def validate(tag, expected_commit=None, expected_tag=None):
    if not re.fullmatch(r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', tag):
        raise ValueError('Use a stable semantic version tag, for example v0.1.3.')
    ref = api(f'git/ref/tags/{tag}')['object']
    if ref['type'] != 'tag':
        raise ValueError('An annotated, verified signed tag is required.')
    obj = api(f'git/tags/{ref["sha"]}')
    if (obj['tag'] != tag or obj['object']['type'] != 'commit'
            or not obj['verification']['verified']):
        raise ValueError('Tag identity, signature or commit is invalid.')
    commit = obj['object']['sha']
    if expected_commit and commit != expected_commit:
        raise ValueError('Tag conflicts with the intended commit; never move it.')
    if expected_tag and ref['sha'] != expected_tag:
        raise ValueError('Tag object changed during preparation.')
    # Check version before installing the Fedora build stack.
    content = run('gh', 'api', '-H', 'Accept: application/vnd.github.raw+json',
                  f'repos/{os.environ["GH_REPO"]}/contents/Cargo.toml?ref={commit}')
    if tomllib.loads(content)['package']['version'] != tag[1:]:
        raise ValueError('Tag does not match Cargo.toml at the intended commit.')
    require_draft(release(tag))
    return {'tag': tag, 'tag_sha': ref['sha'], 'commit_sha': commit}


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def download(asset, destination):
    with destination.open('wb') as stream:
        subprocess.run(['gh', 'api', '-H', 'Accept: application/octet-stream',
                        f'repos/{os.environ["GH_REPO"]}/releases/assets/{asset["id"]}'],
                       stdout=stream, check=True)


def asset_plan(existing, directory):
    """Check every conflict before uploading any missing asset."""
    missing = []
    with tempfile.TemporaryDirectory() as tmp:
        for name in ASSETS:
            matches = [a for a in existing if a['name'] == name]
            if not matches:
                missing.append(name)
                continue
            if len(matches) != 1 or matches[0]['state'] != 'uploaded':
                raise ValueError(f'Incomplete/conflicting asset {name}; inspect the draft recovery guide.')
            saved = Path(tmp) / name
            download(matches[0], saved)
            if digest(saved) != digest(directory / name):
                raise ValueError(f'Conflicting {name}; refusing replacement. Renew author validation after recovery.')
    return missing


def upload(tag, commit, tag_sha, directory):
    identity = validate(tag, commit, tag_sha)
    expected_sum = f'{digest(directory / RPM)}  {RPM}\n'
    if (directory / 'SHA256SUMS').read_text() != expected_sum:
        raise ValueError('Local RPM checksum does not match SHA256SUMS.')
    (directory / 'release-source.json').write_text(json.dumps(identity, sort_keys=True) + '\n')
    item = release(tag)
    require_draft(item)
    if not item:
        # Creation returns the draft identity even before release listings catch
        # up. Never create twice or rediscover the result through a stale list.
        item = json.loads(run(
            'gh', 'api', '--method', 'POST', f'repos/{os.environ["GH_REPO"]}/releases',
            '-f', f'tag_name={tag}', '-f', f'target_commitish={commit}',
            '-f', f'name=open-mpv {tag[1:]}', '-F', 'draft=true',
            '-F', 'generate_release_notes=true'))
        require_draft(item)
        if item['tag_name'] != tag:
            raise ValueError('Created draft does not match the verified tag.')
    existing = list(pages(f'releases/{item["id"]}/assets'))
    missing = asset_plan(existing, directory)
    for name in missing:
        validate(tag, commit, tag_sha)
        current = api(f'releases/{item["id"]}')
        require_draft(current)
        # Keep the release identity through the write too. `gh release upload`
        # would perform another tag lookup instead of using this known draft.
        run('gh', 'api', '--method', 'POST',
            f'https://uploads.github.com/repos/{os.environ["GH_REPO"]}/releases/{item["id"]}/assets?name={name}',
            '-H', 'Content-Type: application/octet-stream',
            '-H', f'Content-Length: {(directory / name).stat().st_size}',
            '--input', str(directory / name))
    summary = (f'Draft: {item["html_url"]}\nCommit: {commit}\nTag: {tag} ({tag_sha})\n'
               f'RPM SHA-256: {digest(directory / RPM)}\n'
               'Download and validate these exact assets before manual publication.\n')
    repo = os.environ.get('GH_REPO', '')
    summary += (f'Checks: https://github.com/{repo}/actions/runs/{os.environ.get("GITHUB_RUN_ID", "")}\n'
                f'Commit: https://github.com/{repo}/commit/{commit}\n'
                f'Tag: https://github.com/{repo}/releases/tag/{tag}\n'
                f'RPM: https://github.com/{repo}/releases/download/{tag}/{RPM}\n'
                f'Checksum: https://github.com/{repo}/releases/download/{tag}/SHA256SUMS\n'
                f'Before publication, download authenticated assets through the draft: {item["html_url"]}\n')
    print(summary)
    if os.getenv('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as stream:
            stream.write(summary)


def restore(tag, commit, tag_sha, directory):
    identity = validate(tag, commit, tag_sha)
    item = release(tag)
    if not item:
        return
    assets = list(pages(f'releases/{item["id"]}/assets'))
    owned = [a for a in assets if a['name'] in ASSETS]
    if not owned:
        return
    # A source manifest alone is safe after an interrupted first upload.
    names = {a['name'] for a in owned}
    if names not in ({'release-source.json'}, set(ASSETS)):
        raise ValueError('Partial draft assets; follow the recovery guide before retrying.')
    if len(owned) != len(names) or any(a['state'] != 'uploaded' for a in owned):
        raise ValueError('Incomplete draft uploads; follow the recovery guide.')
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        saved = Path(tmp)
        for asset in owned:
            download(asset, saved / asset['name'])
        if json.loads((saved / 'release-source.json').read_text()) != identity:
            raise ValueError('Draft source identity conflicts with the verified tag.')
        if names == {'release-source.json'}:
            return
        if (saved / 'SHA256SUMS').read_text() != f'{digest(saved / RPM)}  {RPM}\n':
            raise ValueError('Draft checksum conflicts with its RPM.')
        for name in ASSETS:
            (directory / name).write_bytes((saved / name).read_bytes())
    print('Restored verified draft bytes; package checks will run again.')


def rpm_identity(path):
    fields = run('rpm', '-qp', '--qf', '%{NAME}\n%{ARCH}\n%{EPOCHNUM}\n%{VERSION}\n%{RELEASE}', str(path)).splitlines()
    if len(fields) != 5:
        raise ValueError('Invalid RPM identity.')
    return fields[:2], tuple(fields[2:])


def predecessor(tag, directory):
    import rpm  # Fedora's native epoch/version/release ordering.

    candidate_name, candidate_evr = rpm_identity(directory / RPM)
    best_evr = None
    best_tag = None
    previous = directory / 'previous.rpm'
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / RPM
        for item in pages('releases'):
            if item['draft'] or item['prerelease'] or item['tag_name'] == tag:
                continue
            assets = list(pages(f'releases/{item["id"]}/assets'))
            matches = [a for a in assets if a['name'] == RPM and a['state'] == 'uploaded']
            if len(matches) != 1:
                continue
            download(matches[0], path)
            name, evr = rpm_identity(path)
            if name != candidate_name or not evr[2].endswith('.fc44'):
                continue
            if rpm.labelCompare(evr, candidate_evr) >= 0:
                continue
            if best_evr is None or rpm.labelCompare(evr, best_evr) > 0:
                previous.write_bytes(path.read_bytes())
                best_evr, best_tag = evr, item['tag_name']
    print(f'Previous compatible RPM: {best_tag}' if best_tag else 'No earlier compatible published RPM exists; upgrade/downgrade checks skipped.')
    if best_tag is None:
        previous.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('validate', 'restore', 'upload', 'predecessor'))
    parser.add_argument('--tag', required=True)
    parser.add_argument('--commit')
    parser.add_argument('--tag-sha')
    parser.add_argument('--directory', type=Path, default=Path('dist'))
    args = parser.parse_args()
    if args.command == 'validate':
        identity = validate(args.tag, args.commit, args.tag_sha)
        print(json.dumps(identity))
        if os.getenv('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
                for key, value in identity.items():
                    stream.write(f'{key}={value}\n')
    elif args.command in ('upload', 'restore'):
        if not args.commit or not args.tag_sha:
            parser.error('upload requires --commit and --tag-sha')
        {'upload': upload, 'restore': restore}[args.command](args.tag, args.commit, args.tag_sha, args.directory)
    else:
        predecessor(args.tag, args.directory)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from None
