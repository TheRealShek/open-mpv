#!/usr/bin/env python3
"""Create a focused release PR or validate its exact merged content for signing."""

import argparse
from datetime import date
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import xml.etree.ElementTree as ET
from xml.sax.saxutils import escape

import release

FILES = ('Cargo.toml', 'Cargo.lock', 'packaging/fedora/open-mpv.spec',
         'data/io.github.TheRealShek.OpenMpv.metainfo.xml')
REQUEST = '.github/release-request.json'


def version_tuple(version):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
        raise ValueError('Enter a stable semantic version, for example 0.1.3.')
    return tuple(map(int, version.split('.')))


def validate_text(description):
    if not description.strip() or len(description) > 1000 or any(ord(c) < 32 for c in description):
        raise ValueError('Description must be one nonempty line, at most 1000 characters, without control characters.')


def replace_once(pattern, replacement, text):
    result, count = re.subn(pattern, lambda match: replacement(match), text, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f'Expected exactly one metadata field matching {pattern}.')
    return result


def edits(files, version, description, day):
    """Compute all edits before writing; preserve dependency and release history bytes."""
    version_tuple(version)
    validate_text(description)
    released = date.fromisoformat(day)
    current = tomllib.loads(files['Cargo.toml'])['package']['version']
    if version_tuple(version) <= version_tuple(current):
        raise ValueError('Release version must be newer than the current application version.')
    output = dict(files)
    # TOML's package table is bounded by the next table; only change its version.
    start = output['Cargo.toml'].index('[package]')
    end = output['Cargo.toml'].find('\n[', start)
    if end == -1:
        end = len(output['Cargo.toml'])
    section = replace_once(r'^(version = ")[^"]+(".*)$', lambda m: m[1] + version + m[2], output['Cargo.toml'][start:end])
    output['Cargo.toml'] = output['Cargo.toml'][:start] + section + output['Cargo.toml'][end:]
    output['Cargo.lock'] = replace_once(
        r'(\[\[package\]\]\nname = "open-mpv"\nversion = ")[^"]+("\n)',
        lambda m: m[1] + version + m[2], files['Cargo.lock'])
    output[FILES[2]] = replace_once(r'^(Version:\s*)\S+$', lambda m: m[1] + version, files[FILES[2]])
    # Escape RPM macros even in changelog text. XML is text, never supplied markup.
    entry = (f'* {released.strftime("%a %b %d %Y")} therealshek <TheRealShek@users.noreply.github.com> - {version}-1\n'
             f'- {description.replace("%", "%%")}\n\n')
    output[FILES[2]] = replace_once(r'^%changelog\n', lambda _: '%changelog\n' + entry, output[FILES[2]])
    entry = (f'    <release version="{version}" date="{day}">\n'
             f'      <description><p>{escape(description)}</p></description>\n'
             '    </release>\n')
    output[FILES[3]] = replace_once(r'^  <releases>\n', lambda _: '  <releases>\n' + entry, files[FILES[3]])
    ET.fromstring(output[FILES[3]])
    tomllib.loads(output['Cargo.toml'])
    tomllib.loads(output['Cargo.lock'])
    return output


def git_file(commit, name):
    return subprocess.check_output(['git', 'show', f'{commit}:{name}'], text=True)


def emit(**values):
    print(json.dumps(values))
    if os.getenv('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
            for key, value in values.items():
                stream.write(f'{key}={value}\n')
    if os.getenv('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as stream:
            for key, value in values.items():
                stream.write(f'{key}: {value}\n\n')


def prepare(version, description):
    version_tuple(version)
    validate_text(description)
    tag = f'v{version}'
    release.require_draft(release.release(tag))
    # One active preparation includes a merged PR waiting for publication.
    for item in release.pages('pulls?state=all'):
        if not item['head']['ref'].startswith('release/prepare-v'):
            continue
        if item['head']['repo'] is None or item['head']['repo']['full_name'] != os.environ['GH_REPO']:
            continue
        active_tag = item['head']['ref'].removeprefix('release/prepare-')
        prior_release = release.release(active_tag)
        active = item['state'] == 'open' or (item.get('merged_at') and (not prior_release or prior_release['draft']))
        if not active:
            continue
        if active_tag != tag:
            raise ValueError(f'Finish or close active preparation {item["html_url"]} before requesting another version.')
        # Reuse without resetting the branch or editing the owner's PR.
        emit(pr=item['html_url'], message='Existing preparation retained; review its current edits. Merge or resume signing by PR number.')
        return
    base = release.run('git', 'rev-parse', 'HEAD')
    branch = f'release/prepare-{tag}'
    refs = list(release.pages('git/matching-refs/heads/release/prepare-'))
    for ref in refs:
        if ref['ref'] == f'refs/heads/{branch}':
            raise ValueError('Preparation branch exists without an active PR; inspect it and open its PR manually. No files were overwritten.')
    files = {name: Path(name).read_text() for name in FILES}
    day = date.today().isoformat()
    output = edits(files, version, description, day)
    release.run('git', 'switch', '-c', branch)
    for name, content in output.items():
        Path(name).write_text(content)
    Path(REQUEST).write_text(json.dumps({'base': base, 'version': version, 'description': description, 'date': day}, indent=2) + '\n')
    release.run('git', 'add', *FILES, REQUEST)
    release.run('git', '-c', 'user.name=github-actions[bot]', '-c', 'user.email=41898282+github-actions[bot]@users.noreply.github.com',
                'commit', '-m', f'chore(release): prepare {tag}')
    release.run('git', 'push', 'origin', f'HEAD:refs/heads/{branch}')
    template = Path('.github/pull_request_template.md').read_text()
    body = template + (f'\n**Release preparation:** {tag}\n\n{description}\n\n'
                       'Metadata only. Approve workflows to run if GitHub requests it; review required CI before merging. '
                       'After merge, approve the protected signing job. Publication remains manual after exact RPM validation.\n')
    with tempfile.NamedTemporaryFile(mode='w', suffix='.md') as stream:
        stream.write(body)
        stream.flush()
        url = release.run('gh', 'pr', 'create', '--base', 'main', '--head', branch,
                          '--title', f'chore(release): prepare {tag}', '--body-file', stream.name)
    emit(pr=url, base=base, tag=tag)


def merged(number):
    item = release.api(f'pulls/{number}')
    if (not item['merged'] or item['base']['ref'] != 'main'
            or not item['head']['ref'].startswith('release/prepare-v')
            or item['head']['repo'] is None
            or item['head']['repo']['full_name'] != os.environ['GH_REPO']):
        raise ValueError('Signing requires a merged preparation PR into main in this repository.')
    commit = item['merge_commit_sha']
    subprocess.run(['git', 'fetch', 'origin', commit, item['head']['sha']], check=True)
    request = json.loads(git_file(commit, REQUEST))
    version = request['version']
    version_tuple(version)
    tag = f'v{version}'
    if item['head']['ref'] != f'release/prepare-{tag}':
        raise ValueError('Preparation branch and merged version disagree.')
    release.require_draft(release.release(tag))
    base = request['base']
    if not re.fullmatch('[0-9a-f]{40}', base):
        raise ValueError('Invalid preparation base.')
    subprocess.run(['git', 'fetch', 'origin', base], check=True)
    subprocess.run(['git', 'merge-base', '--is-ancestor', base, commit], check=True)
    files = {name: git_file(base, name) for name in FILES}
    expected = edits(files, version, request['description'], request['date'])
    changed = set(release.run('git', 'diff', '--name-only', base, item['head']['sha']).splitlines())
    if changed != set(FILES) | {REQUEST}:
        raise ValueError('Preparation PR contains unexpected file changes.')
    for name, content in expected.items():
        if git_file(commit, name) != content or git_file(item['head']['sha'], name) != content:
            raise ValueError(f'Merged preparation metadata differs from the reviewed request: {name}.')
    if git_file(commit, REQUEST) != git_file(item['head']['sha'], REQUEST):
        raise ValueError('Merged release request differs from reviewed PR.')
    checks = release.api(f'commits/{item["head"]["sha"]}/check-runs')['check_runs']
    required = [check for check in checks if check['name'] == 'Required checks' and check['app']['slug'] == 'github-actions']
    if not required or required[0]['conclusion'] != 'success':
        raise ValueError('Required checks must succeed on the reviewed preparation head.')
    # Refuse an existing tag unless it is verified and points to this exact merge.
    refs = release.api(f'git/matching-refs/tags/{tag}')
    if any(ref['ref'] == f'refs/tags/{tag}' for ref in refs):
        release.validate(tag, commit)
    emit(commit=commit, tag=tag, pr=item['html_url'],
         commit_url=f'https://github.com/{os.environ["GH_REPO"]}/commit/{commit}',
         checks=f'{item["html_url"]}/checks')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('prepare', 'merged'))
    parser.add_argument('--version')
    parser.add_argument('--description')
    parser.add_argument('--pr', type=int)
    args = parser.parse_args()
    if args.command == 'prepare':
        if not args.version or not args.description:
            parser.error('prepare requires --version and --description')
        prepare(args.version, args.description)
    else:
        if not args.pr or args.pr < 1:
            parser.error('merged requires a positive --pr number')
        merged(args.pr)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from None
