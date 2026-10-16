#!/usr/bin/env python3
"""Release notes for a tag: the hand-written highlights in docs/releases/<tag>.md, the downloads
with their checksums, and the commits since the previous tag grouped by Conventional Commits type.

    .github/scripts/release_notes.py TAG ARCHIVE... > notes.md
"""
import hashlib
import os
import re
import subprocess
import sys

REPO = 'redstone-md/rava'
GROUPS = [
    ('feat', 'Features'),
    ('perf', 'Performance'),
    ('fix', 'Fixes'),
    ('refactor', 'Refactoring'),
    ('docs', 'Documentation'),
    ('test', 'Tests'),
    ('bench', 'Benchmarks'),
    ('build', 'Build and CI'),
    ('ci', 'Build and CI'),
    ('chore', 'Maintenance'),
]
# What each archive runs on, by the platform in its name.
PLATFORMS = [
    ('linux-x86_64', 'Linux x86-64', 'glibc 2.35 or newer'),
    ('macos-x86_64', 'macOS x86-64', 'Intel; Apple Silicon through Rosetta 2'),
    ('windows-x86_64', 'Windows x86-64', 'Windows 10 or newer'),
]
SUBJECT = re.compile(r'^(\w+)(?:\(([^)]*)\))?(!)?: (.+)$')


def git(*args):
    return subprocess.run(['git', *args], check=True, capture_output=True, text=True).stdout.strip()


def revision(tag):
    """The tag's commit, or the checked-out one when the release is yet to create the tag."""
    try:
        git('rev-parse', '--verify', '--quiet', f'refs/tags/{tag}')
        return tag
    except subprocess.CalledProcessError:
        return 'HEAD'


def previous_tag(rev):
    try:
        return git('describe', '--tags', '--abbrev=0', f'{rev}^')
    except subprocess.CalledProcessError:
        return None


def unmention(text):
    """Puts `@Name` outside code into code: GitHub takes it for a user mention (and lists that
    account among the release's contributors), as a Java annotation in a subject would be."""
    parts = text.split('`')
    for i in range(0, len(parts), 2):
        parts[i] = re.sub(r'(?<![\w@])@([A-Za-z][\w.-]*)', r'`@\1`', parts[i])
    return '`'.join(parts)


def commits(rng):
    out = git('log', '--no-merges', '--format=%h%x00%s', rng)
    return [(sha, unmention(subject)) for sha, subject in (line.split('\0', 1) for line in out.splitlines() if line)]


def grouped(entries):
    groups, other = {}, []
    for sha, subject in entries:
        m = SUBJECT.match(subject)
        title = next((t for k, t in GROUPS if m and m.group(1) == k), None)
        if title is None:
            other.append((sha, subject))
            continue
        scope = f'**{m.group(2)}:** ' if m.group(2) else ''
        groups.setdefault(title, []).append((sha, scope + m.group(4)))
    order = list(dict.fromkeys(t for _, t in GROUPS))
    result = [(t, groups[t]) for t in order if t in groups]
    if other:
        result.append(('Other changes', other))
    return result


def stem(name):
    for ext in ('.tar.gz', '.zip'):
        if name.endswith(ext):
            return name[:-len(ext)]
    return name


def main():
    tag, archives = sys.argv[1], sys.argv[2:]
    version = tag[1:] if tag.startswith('v') else tag
    rev = revision(tag)
    prev = previous_tag(rev)
    entries = commits(f'{prev}..{rev}' if prev else rev)
    out = []

    highlights = f'docs/releases/{tag}.md'
    if os.path.exists(highlights):
        out.append(open(highlights).read().strip())
        out.append('')

    out += ['## Download', '', '| Platform | Archive | Size | SHA-256 |', '|---|---|---:|---|']
    rows = []
    for archive in archives:
        name = os.path.basename(archive)
        sha = hashlib.sha256(open(archive, 'rb').read()).hexdigest()
        size = os.path.getsize(archive) / (1 << 20)
        order, label, note = next(((i, l, n) for i, (key, l, n) in enumerate(PLATFORMS) if key in name), (len(PLATFORMS), name, ''))
        url = f'https://github.com/{REPO}/releases/download/{tag}/{name}'
        rows.append((order, f'| {label} ({note}) | [`{name}`]({url}) | {size:.0f} MiB | `{sha[:16]}…` |'))
    out += [row for _, row in sorted(rows)]
    out += [
        '',
        'Each archive is a complete Java runtime: the `rava` VM and the Java 21 class library it runs '
        '(OpenJDK, GPLv2 with the Classpath Exception, notices in `legal/`). No JDK needs to be installed. '
        'Full checksums are in the `.sha256` files beside the archives.',
        '',
        '```sh',
        '# Linux (on a Mac, macos-x86_64)',
        f'tar xzf rava-{version}-linux-x86_64.tar.gz',
        f'rava-{version}-linux-x86_64/bin/rava -version',
        '```',
        '',
        '```powershell',
        '# Windows',
        f'Expand-Archive rava-{version}-windows-x86_64.zip .',
        f'.\\rava-{version}-windows-x86_64\\bin\\rava.exe -version',
        '```',
        '',
        'Then type `rava` wherever you would type `java`.',
        '',
        '## Changes',
        '',
    ]
    if prev:
        out.append(f'{len(entries)} commits since {prev}. '
                   f'Full diff: [`{prev}...{tag}`](https://github.com/{REPO}/compare/{prev}...{tag})')
    else:
        out.append(f'The first release: {len(entries)} commits.')
    out.append('')
    for title, items in grouped(entries):
        out.append(f'<details><summary><b>{title}</b> ({len(items)})</summary>')
        out.append('')
        out += [f'- {text} ({sha})' for sha, text in items]
        out.append('')
        out.append('</details>')
        out.append('')
    print('\n'.join(out).rstrip() + '\n')


if __name__ == '__main__':
    main()
