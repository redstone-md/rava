#!/usr/bin/env python3
"""Release notes for a tag: the hand-written highlights in docs/releases/<tag>.md, the download
with its checksum, and the commits since the previous tag grouped by Conventional Commits type.

    .github/scripts/release_notes.py TAG ARCHIVE > notes.md
"""
import hashlib
import os
import re
import subprocess
import sys

REPO = 'redstone-md/rava'
GROUPS = [
    ('feat', '✨ Features'),
    ('perf', '⚡ Performance'),
    ('fix', '🐛 Fixes'),
    ('refactor', '🧹 Refactoring'),
    ('docs', '📚 Documentation'),
    ('test', '🧪 Tests'),
    ('bench', '📊 Benchmarks'),
    ('build', '📦 Build and CI'),
    ('ci', '📦 Build and CI'),
    ('chore', '🔧 Maintenance'),
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


def commits(rng):
    out = git('log', '--no-merges', '--format=%h%x00%s', rng)
    return [line.split('\0', 1) for line in out.splitlines() if line]


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


def main():
    tag, archive = sys.argv[1], sys.argv[2]
    name = os.path.basename(archive)
    sha = hashlib.sha256(open(archive, 'rb').read()).hexdigest()
    size = os.path.getsize(archive) / (1 << 20)
    rev = revision(tag)
    prev = previous_tag(rev)
    entries = commits(f'{prev}..{rev}' if prev else rev)
    out = []

    highlights = f'docs/releases/{tag}.md'
    if os.path.exists(highlights):
        out.append(open(highlights).read().strip())
        out.append('')

    out += [
        '## 📥 Download',
        '',
        '| Platform | Archive | Size |',
        '|---|---|---:|',
        f'| Linux x86-64 (glibc 2.35+) | [`{name}`](https://github.com/{REPO}/releases/download/{tag}/{name}) | {size:.0f} MiB |',
        '',
        f'SHA-256: `{sha}`',
        '',
        'The archive is a complete Java runtime: the `rava` VM and the Java 21 class library it runs '
        '(OpenJDK, GPLv2 with the Classpath Exception, notices in `legal/`). No JDK needs to be installed.',
        '',
        '```sh',
        f'curl -LO https://github.com/{REPO}/releases/download/{tag}/{name}',
        f'tar xzf {name}',
        f'{name[:-len(".tar.gz")]}/bin/rava -version',
        f'{name[:-len(".tar.gz")]}/bin/rava -jar server.jar --nogui   # wherever you would type java',
        '```',
        '',
        f'## 📝 Changes',
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
