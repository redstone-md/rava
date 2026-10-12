#!/usr/bin/env python3
"""Sets up a Minecraft client as the official launcher would, and runs it.

    bench/mc_client.py [-v VERSION] [-d DIR] [--loader fabric|neoforge] [--mods SLUG,...]
                       [--sounds] [--world NAME] [--run BINARY [ARGS...]]

Downloads the version's client jar, its libraries (with the Linux natives extracted into
DIR/natives) and its assets (sounds only with --sounds) into DIR (default
/tmp/claude-0/mc-client). With --loader, the newest Fabric loader or NeoForge for the version
is installed on top (NeoForge through its installer, run with `java`), and --mods adds mods from
Modrinth into DIR/game/mods. Prints the command line a launcher would run, or with --run
starts it with BINARY (`java` or rava) in offline mode (no account: singleplayer works);
--world NAME enters that save directly.
"""
import argparse, hashlib, json, os, re, shlex, subprocess, sys, urllib.parse, urllib.request, zipfile

MANIFEST = 'https://piston-meta.mojang.com/mc/game/version_manifest_v2.json'
RESOURCES = 'https://resources.download.minecraft.net'
FABRIC = 'https://meta.fabricmc.net/v2/versions/loader'
NEOFORGE = 'https://maven.neoforged.net/releases/net/neoforged/neoforge'
MODRINTH = 'https://api.modrinth.com/v2'

ap = argparse.ArgumentParser()
ap.add_argument('-v', default='1.21.1')
ap.add_argument('-d', default='/tmp/claude-0/mc-client')
ap.add_argument('--loader', choices=['fabric', 'neoforge'])
ap.add_argument('--mods', default='')
ap.add_argument('--sounds', action='store_true')
ap.add_argument('--world')
ap.add_argument('--run', nargs=argparse.REMAINDER)
args = ap.parse_args()
os.makedirs(args.d, exist_ok=True)


def fetch(url, path, sha1=None):
    if os.path.exists(path) and (sha1 is None or hashlib.sha1(open(path, 'rb').read()).hexdigest() == sha1):
        return path
    os.makedirs(os.path.dirname(path), exist_ok=True)
    for attempt in range(5):
        try:
            data = urllib.request.urlopen(urllib.request.Request(url, headers={'User-Agent': 'rava-bench'}), timeout=120).read()
            break
        except Exception:
            if attempt == 4:
                raise
    open(path, 'wb').write(data)
    return path


def get_json(url):
    return json.load(urllib.request.urlopen(urllib.request.Request(url, headers={'User-Agent': 'rava-bench'}), timeout=60))


def allowed(rules):
    ok = not rules
    for r in rules or []:
        if 'os' in r and r['os'].get('name') not in (None, 'linux'):
            continue
        if 'features' in r:
            continue
        ok = r['action'] == 'allow'
    return ok


def maven_path(name):
    """`group:artifact:version[:classifier][@ext]` as a repository path."""
    coords, _, ext = name.partition('@')
    parts = coords.split(':')
    group, artifact, version = parts[:3]
    classifier = f'-{parts[3]}' if len(parts) > 3 else ''
    return f"{group.replace('.', '/')}/{artifact}/{version}/{artifact}-{version}{classifier}.{ext or 'jar'}"


def version_json(vid):
    """A version's JSON, merged over the one it inherits from (libraries first, arguments after)."""
    path = f'{args.d}/versions/{vid}/{vid}.json'
    if not os.path.exists(path):
        manifest = json.load(open(fetch(MANIFEST, f'{args.d}/manifest.json')))
        entry = next(v for v in manifest['versions'] if v['id'] == vid)
        fetch(entry['url'], path, entry['sha1'])
    v = json.load(open(path))
    if 'inheritsFrom' not in v:
        return v
    base = version_json(v['inheritsFrom'])
    merged = dict(base)
    merged['mainClass'] = v.get('mainClass', base['mainClass'])
    merged['libraries'] = v.get('libraries', []) + base['libraries']
    merged['arguments'] = {k: base['arguments'].get(k, []) + v.get('arguments', {}).get(k, []) for k in ('jvm', 'game')}
    merged['id'] = v['id']
    return merged


vid = args.v
if args.loader == 'fabric':
    loader = next(l for l in get_json(f'{FABRIC}/{args.v}') if l['loader']['stable'])['loader']['version']
    vid = f'fabric-loader-{loader}-{args.v}'
    path = f'{args.d}/versions/{vid}/{vid}.json'
    if not os.path.exists(path):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        json.dump(get_json(f'{FABRIC}/{args.v}/{loader}/profile/json'), open(path, 'w'))
elif args.loader == 'neoforge':
    meta = urllib.request.urlopen(f'{NEOFORGE}/maven-metadata.xml', timeout=60).read().decode()
    prefix = '.'.join(args.v.split('.')[1:]) + '.'
    neo = [v for v in re.findall(r'<version>([^<]+)</version>', meta) if v.startswith(prefix) and 'beta' not in v][-1]
    vid = f'neoforge-{neo}'
    if not os.path.exists(f'{args.d}/versions/{vid}/{vid}.json'):
        version_json(args.v)
        fetch(json.load(open(f'{args.d}/versions/{args.v}/{args.v}.json'))['downloads']['client']['url'], f'{args.d}/versions/{args.v}/{args.v}.jar')
        installer = fetch(f'{NEOFORGE}/{neo}/neoforge-{neo}-installer.jar', f'{args.d}/installers/neoforge-{neo}-installer.jar')
        profiles = f'{args.d}/launcher_profiles.json'
        if not os.path.exists(profiles):
            json.dump({'profiles': {}}, open(profiles, 'w'))
        subprocess.check_call(['java', '-jar', installer, '--installClient', args.d], stdout=subprocess.DEVNULL)

version = version_json(vid)
base = version_json(args.v)
client = fetch(base['downloads']['client']['url'], f'{args.d}/versions/{args.v}/{args.v}.jar', base['downloads']['client']['sha1'])

classpath, natives = [], f'{args.d}/natives'
os.makedirs(natives, exist_ok=True)
for lib in version['libraries']:
    if not allowed(lib.get('rules')):
        continue
    art = lib.get('downloads', {}).get('artifact')
    if art and art.get('url'):
        path = fetch(art['url'], f"{args.d}/libraries/{art['path']}", art.get('sha1'))
    elif art:
        path = f"{args.d}/libraries/{art['path']}"
        if not os.path.exists(path):
            continue
    elif 'url' in lib:
        rel = maven_path(lib['name'])
        path = fetch(lib['url'].rstrip('/') + '/' + rel, f'{args.d}/libraries/{rel}', lib.get('sha1'))
    else:
        continue
    if lib['name'].endswith('natives-linux'):
        with zipfile.ZipFile(path) as z:
            for n in z.namelist():
                if n.endswith('.so'):
                    open(f'{natives}/{os.path.basename(n)}', 'wb').write(z.read(n))
    if path not in classpath:
        classpath.append(path)
if args.loader != 'neoforge':
    classpath.append(client)

index_info = base['assetIndex']
index = json.load(open(fetch(index_info['url'], f"{args.d}/assets/indexes/{index_info['id']}.json", index_info['sha1'])))
objects = [(k, o) for k, o in index['objects'].items() if args.sounds or not k.startswith('minecraft/sounds/')]
for i, (name, o) in enumerate(objects):
    h = o['hash']
    fetch(f'{RESOURCES}/{h[:2]}/{h}', f'{args.d}/assets/objects/{h[:2]}/{h}', h)

game_dir = f'{args.d}/game'
os.makedirs(f'{game_dir}/mods', exist_ok=True)
for slug in filter(None, args.mods.split(',')):
    q = urllib.parse.urlencode({'game_versions': json.dumps([args.v]), 'loaders': json.dumps([args.loader])})
    files = get_json(f'{MODRINTH}/project/{slug}/version?{q}')[0]['files']
    f = next((f for f in files if f['primary']), files[0])
    fetch(f['url'], f"{game_dir}/mods/{f['filename']}", f['hashes']['sha1'])

values = {
    'auth_player_name': 'rava', 'version_name': vid, 'game_directory': game_dir,
    'assets_root': f'{args.d}/assets', 'assets_index_name': index_info['id'],
    'auth_uuid': '00000000-0000-0000-0000-000000000000', 'auth_access_token': '0',
    'clientid': '0', 'auth_xuid': '0', 'user_type': 'legacy', 'version_type': 'release',
    'natives_directory': natives, 'launcher_name': 'rava', 'launcher_version': '1',
    'classpath': ':'.join(classpath), 'library_directory': f'{args.d}/libraries',
    'classpath_separator': ':',
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


cmd = ['-Xmx2G'] + expand(version['arguments']['jvm']) + [version['mainClass']] + expand(version['arguments']['game'])
if args.world:
    cmd += ['--quickPlaySingleplayer', args.world]
if args.run:
    os.chdir(game_dir)
    sys.exit(subprocess.call(args.run + cmd))
print(' '.join(shlex.quote(c) for c in cmd))
