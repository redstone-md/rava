#!/usr/bin/env python3
"""Sets up a vanilla Minecraft client as the official launcher would, and runs it.

    bench/mc_client.py [-v VERSION] [-d DIR] [--sounds] [--run BINARY [ARGS...]]

Downloads the version's client jar, its libraries (with the Linux natives extracted into
DIR/natives) and its assets (sounds only with --sounds) into DIR (default
/tmp/claude-0/mc-client), then prints the command line a launcher would run, or with --run
starts it with BINARY (`java` or rava) in offline mode (no account: singleplayer works).
"""
import argparse, hashlib, json, os, shlex, subprocess, sys, urllib.request, zipfile

MANIFEST = 'https://piston-meta.mojang.com/mc/game/version_manifest_v2.json'
RESOURCES = 'https://resources.download.minecraft.net'

ap = argparse.ArgumentParser()
ap.add_argument('-v', default='1.21.1')
ap.add_argument('-d', default='/tmp/claude-0/mc-client')
ap.add_argument('--sounds', action='store_true')
ap.add_argument('--run', nargs=argparse.REMAINDER)
args = ap.parse_args()
os.makedirs(args.d, exist_ok=True)


def fetch(url, path, sha1=None):
    if os.path.exists(path) and (sha1 is None or hashlib.sha1(open(path, 'rb').read()).hexdigest() == sha1):
        return path
    os.makedirs(os.path.dirname(path), exist_ok=True)
    for attempt in range(5):
        try:
            data = urllib.request.urlopen(url, timeout=60).read()
            break
        except Exception as e:
            if attempt == 4:
                raise
    open(path, 'wb').write(data)
    return path


def allowed(rules):
    ok = not rules
    for r in rules or []:
        if 'os' in r and r['os'].get('name') not in (None, 'linux'):
            continue
        if 'features' in r:
            continue
        ok = r['action'] == 'allow'
    return ok


manifest = json.load(open(fetch(MANIFEST, f'{args.d}/manifest.json')))
entry = next(v for v in manifest['versions'] if v['id'] == args.v)
version = json.load(open(fetch(entry['url'], f'{args.d}/versions/{args.v}.json', entry['sha1'])))
client = fetch(version['downloads']['client']['url'], f'{args.d}/versions/{args.v}.jar', version['downloads']['client']['sha1'])

classpath, natives = [], f'{args.d}/natives'
os.makedirs(natives, exist_ok=True)
for lib in version['libraries']:
    if not allowed(lib.get('rules')):
        continue
    art = lib.get('downloads', {}).get('artifact')
    if not art:
        continue
    path = fetch(art['url'], f"{args.d}/libraries/{art['path']}", art['sha1'])
    if 'natives-linux' in lib['name'] and lib['name'].endswith('natives-linux'):
        with zipfile.ZipFile(path) as z:
            for n in z.namelist():
                if n.endswith('.so'):
                    open(f'{natives}/{os.path.basename(n)}', 'wb').write(z.read(n))
    classpath.append(path)
classpath.append(client)

index_info = version['assetIndex']
index = json.load(open(fetch(index_info['url'], f"{args.d}/assets/indexes/{index_info['id']}.json", index_info['sha1'])))
objects = [(k, o) for k, o in index['objects'].items() if args.sounds or not k.startswith('minecraft/sounds/')]
for i, (name, o) in enumerate(objects):
    h = o['hash']
    fetch(f'{RESOURCES}/{h[:2]}/{h}', f'{args.d}/assets/objects/{h[:2]}/{h}', h)
    if i % 500 == 0:
        print(f'assets {i}/{len(objects)}', file=sys.stderr)

game_dir = f'{args.d}/game'
os.makedirs(game_dir, exist_ok=True)
values = {
    'auth_player_name': 'rava', 'version_name': args.v, 'game_directory': game_dir,
    'assets_root': f'{args.d}/assets', 'assets_index_name': index_info['id'],
    'auth_uuid': '00000000-0000-0000-0000-000000000000', 'auth_access_token': '0',
    'clientid': '0', 'auth_xuid': '0', 'user_type': 'legacy', 'version_type': 'release',
    'natives_directory': natives, 'launcher_name': 'rava', 'launcher_version': '1',
    'classpath': ':'.join(classpath),
}


def expand(items):
    out = []
    for a in items:
        if isinstance(a, dict):
            if not allowed(a.get('rules')):
                continue
            a = a['value']
        for s in (a if isinstance(a, list) else [a]):
            for k, v in values.items():
                s = s.replace('${' + k + '}', v)
            out.append(s)
    return out


jvm = ['-Xmx2G'] + expand(version['arguments']['jvm'])
game = expand(version['arguments']['game'])
cmd = jvm + [version['mainClass']] + game
if args.run:
    os.chdir(game_dir)
    sys.exit(subprocess.call(args.run + cmd))
print(' '.join(shlex.quote(c) for c in cmd))
