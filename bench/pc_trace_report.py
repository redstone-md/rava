"""Bucket a [PC-TRACE] stream into the boot's phase windows and rank each one.

The trace carries one line per 10 ms sample:  epoch_ms | leaf | verified chain | stack frames.
Window boundaries come from the guest's own log lines (1 s resolution), except the
residue->ring edge, which is not a log line and is found in the trace itself.
"""
import re, os, sys, datetime, collections

# Directory holding <tag>.log (guest stdout) and <tag>.log.err (the [PC-TRACE] stream).
SP = os.environ.get('RAVA_PC_TRACE_DIR', '.')

# ---------- rust symbol demangling (legacy _ZN..E and v0 _R..) ----------
ESC = {'$LT$': '<', '$GT$': '>', '$u20$': ' ', '$C$': ',', '$RF$': '&', '$BP$': '*',
       '$LP$': '(', '$RP$': ')', '$u7b$': '{', '$u7d$': '}', '$u27$': "'", '$SP$': '@',
       '$u5b$': '[', '$u5d$': ']', '$u7e$': '~', '$u3b$': ';', '$u2b$': '+', '$u21$': '!'}
HASH = re.compile(r'^h[0-9a-f]{16}$')


def unesc(s):
    for k, v in ESC.items():
        s = s.replace(k, v)
    return s.replace('..', '::')


def demangle(sym):
    s = sym
    if s.endswith('E') and (s.startswith('ZN') or s.startswith('_ZN')):
        body = s[3:-1] if s.startswith('_ZN') else s[2:-1]
        parts, i = [], 0
        while i < len(body):
            j = i
            while j < len(body) and body[j].isdigit():
                j += 1
            if j == i:
                break
            n = int(body[i:j])
            parts.append(body[j:j + n])
            i = j + n
        if parts and HASH.match(parts[-1]):
            parts.pop()
        if parts:
            return unesc('::'.join(parts))
    # v0: _RNvCs<id>_<crate><path>  -- crude: strip the disambiguator run
    if s.startswith('_R') or s.startswith('RNv') or s.startswith('RIN'):
        t = re.sub(r'^_?R[A-Za-z]*Cs[0-9A-Za-z]+_', '', s)
        t = re.sub(r'\d+', ' ', t)
        return ' '.join(t.split())
    return s


def short(sym):
    """Readable, aggregation-stable name: keep the tail of the path, drop <impl> noise."""
    if sym.startswith('<jit> '):
        return sym
    d = demangle(re.sub(r'\.llvm\.\d+$', '', sym))
    d = re.sub(r'<(.+?) as (.+?)>', r'\1', d)
    d = strip_generics(d)
    segs = [x for x in d.split('::') if x and not x.startswith('{{')]
    if len(segs) > 3:
        segs = [segs[0]] + segs[-2:]
    return '::'.join(segs)


def strip_generics(s):
    """Drop balanced <...> argument lists (they contain :: and would break path splitting)."""
    out, depth = [], 0
    for ch in s:
        if ch == '<':
            depth += 1
        elif ch == '>':
            depth = max(0, depth - 1)
        elif depth == 0:
            out.append(ch)
    return ''.join(out)


# ---------- load ----------
def load(tag):
    trace = []
    with open(f'{SP}\\{tag}.log.err', 'r', encoding='utf-8', errors='replace') as f:
        for ln in f:
            if not ln.startswith('[PC-TRACE] '):
                continue
            rest = ln[11:].rstrip('\n')
            sp = rest.find(' ')
            t = int(rest[:sp])
            fields = rest[sp + 1:].split(' | ')
            leaf = fields[0]
            chain = fields[1].split(' < ') if len(fields) > 1 and fields[1] else []
            frames = fields[2].split(';') if len(fields) > 2 and fields[2] else []
            trace.append((t, leaf, chain, frames))
    trace.sort(key=lambda r: r[0])
    return trace


def log_marks(tag):
    """local [HH:MM:SS] of each phase marker -> epoch ms (start of that second)."""
    marks = {}
    day = None
    with open(f'{SP}\\{tag}.log', 'r', encoding='utf-8', errors='replace') as f:
        lines = f.read().splitlines()
    # MC's log stamps are UTC (verified: they trail local wall clock by the TZ offset).
    mtime = datetime.datetime.fromtimestamp(os.path.getmtime(f'{SP}\\{tag}.log'),
                                            datetime.timezone.utc)
    day = mtime.date()
    pats = [('env', 'Environment: Environment['),
            ('start', 'Starting minecraft server version'),
            ('prep', 'Preparing level'),
            ('done', 'Done (')]
    for ln in lines:
        m = re.match(r'\[(\d\d):(\d\d):(\d\d)\]', ln)
        if not m:
            continue
        h, mi, s = (int(x) for x in m.groups())
        dt = datetime.datetime.combine(day, datetime.time(h, mi, s),
                                       tzinfo=datetime.timezone.utc)
        ep = int(dt.timestamp() * 1000)
        for key, pat in pats:
            if pat in ln and key not in marks:
                marks[key] = ep
        if 'first' not in marks:
            marks['first'] = ep
    return marks


# ---------- ranking ----------
def table(rows, den, top=15, label=''):
    out = [f'--- {label} (n={den}) ---']
    for name, n in rows[:top]:
        out.append(f'{n * 100.0 / max(den, 1):5.1f}%  {n:6d}  {name}')
    return '\n'.join(out)


def rank(counter, den, top=15, label=''):
    rows = sorted(counter.items(), key=lambda kv: (-kv[1], kv[0]))
    return table(rows, den, top, label)


def window(trace, lo, hi):
    return [r for r in trace if lo <= r[0] < hi]


def analyse(name, samples, top=15):
    n = len(samples)
    if n == 0:
        return f'### {name}: NO SAMPLES'
    inside = sum(1 for r in samples if r[1].startswith('<jit> '))
    leaves = collections.Counter(short(r[1]) for r in samples)
    # caller attribution: prefer the proven chain head, fall back to the innermost frame
    callers = collections.Counter()
    for _, leaf, chain, frames in samples:
        c = chain[0] if chain else (frames[0] if frames else '<none>')
        callers[f'{short(leaf)}  <-  {short(c)}'] += 1
    anywhere = collections.Counter()
    for _, leaf, chain, frames in samples:
        seen = set()
        for f in frames:
            s = short(f)
            if s not in seen:
                seen.add(s)
                anywhere[s] += 1
    out = [f'### {name}: {n} samples ~{n * 0.01:.1f}s sampled, '
           f'INSIDE COMPILED CODE {inside * 100.0 / n:.1f}% ({inside}), host {100 - inside * 100.0 / n:.1f}%']
    out.append(rank(leaves, n, top, 'LEAF (self-time)'))
    out.append(rank(callers, n, top, 'LEAF <- CALLER (proven where possible)'))
    out.append(rank(anywhere, n, top, 'ON STACK ANYWHERE (conservative superset)'))
    return '\n'.join(out)



"""Final per-window report: top-15 leaves with the dominant caller for each, plus clusters."""

CLUSTERS = [
    ('compiled code (<jit>)', lambda s: s.startswith('<jit> ')),
    ('cranelift/regalloc COMPILE', lambda s: any(k in s for k in
        ('cranelift', 'regalloc2', 'isle', 'Verifier', 'EgraphPass', 'MachBuffer'))),
    ('allocator (mimalloc + RawVec)', lambda s: any(k in s for k in
        ('mi_', 'rust_alloc', 'rust_dealloc', 'rust_realloc', 'RawVec', 'do_reserve',
         'finish_grow', 'try_allocate_in', 'alloc::Global'))),
    ('GC / arena barriers', lambda s: any(k in s for k in
        ('Heap::gc', 'Vm::gc', 'maybe_gc', 'retire_object_slot', 'forward_if_moved',
         'arena_root_locations', 'arena_collect_jit', 'mark_', 'major_collect', 'ref_mask_of'))),
    ('dispatch host path', lambda s: any(k in s for k in
        ('jit_direct', 'jit_call', 'jit_anewarray', 'dispatch_miss', 'lambda_fp', 'jit_indy',
         'fu_invoke', 'fu_fastpath', 'fu_double_view', 'jit_new', 'jit_instanceof', 'jit_getfield',
         'resolve_method', 'direct_mono', 'jit_invoke', 'rava_getfield', 'rava_arraylength'))),
    ('name-keyed lookup / hashing', lambda s: any(k in s for k in
        ('resolve_field_id', 'field_key', 'tlcache', 'get_or_load', 'super_chain', 'owner_bits',
         'hash_one', 'Hasher', 'structural_key', 'combo_hash', 'key_intern', 'intern_field',
         'java_string_hash', 'memcmp', 'HashMap', 'hashbrown', 'IndexMap', 'indexmap'))),
    ('thread-local access', lambda s: 'LocalKey' in s),
    ('string / substring search', lambda s: any(k in s for k in
        ('memchr', 'next_match', 'CharSearcher', 'is_contained_in', 'char_len', 'value_to_string',
         'StringifyMemo', 'MemoCache', 'format_inner', 'write_str', 'Display', 'memmove'))),
    ('interpreter opcodes / frames', lambda s: any(k in s for k in
        ('opcodes::', 'Vm::invoke', 'Frame::', 'frames::', 'Vm::apply', 'natives::', 'stream::',
         'reflect::', 'map::dispatch', 'Heap::get_field', 'Heap::put_field'))),
    ('env / registry reads', lambda s: any(k in s for k in
        ('EnvironmentVariable', 'var_os', 'to_u16s'))),
    ('kernel / file / zip', lambda s: any(k in s for k in
        ('Nt', 'Zw', 'miniz', 'flate2', 'zip::', 'Rtl', 'Wait', 'CxxFrame', '_NLG'))),
]


def cluster_of(s):
    for name, f in CLUSTERS:
        if f(s):
            return name
    return 'other'


def report(name, samples, top=15):
    n = len(samples)
    if not n:
        print(f'\n## {name}: NO SAMPLES\n')
        return
    inside = sum(1 for r in samples if r[1].startswith('<jit> '))
    leaves = collections.Counter(short(r[1]) for r in samples)
    callers = collections.defaultdict(collections.Counter)
    for _, leaf, chain, frames in samples:
        L = short(leaf)
        if chain:
            callers[L][short(chain[0])] += 1
    print(f'\n## {name}')
    print(f'{n} samples (~{n*0.01:.1f}s sampled) | **inside compiled code {inside*100.0/n:.1f}%** '
          f'| host {100-inside*100.0/n:.1f}%\n')
    print('| % | n | leaf symbol (self-time) | dominant VERIFIED caller (share of that leaf) |')
    print('|---:|---:|---|---|')
    for sym, c in leaves.most_common(top):
        cc = callers.get(sym)
        if cc:
            k, v = cc.most_common(1)[0]
            att = f'`{k}` ({v*100//c}%)'
        else:
            att = '_unprovable (indirect/IAT call)_'
        print(f'| {c*100.0/n:.1f}% | {c} | `{sym}` | {att} |')
    cl = collections.Counter()
    for _, leaf, _, _ in samples:
        cl[cluster_of(short(leaf))] += 1
    print('\n| % | n | cluster (leaf self-time, partition) |')
    print('|---:|---:|---|')
    for k, v in cl.most_common():
        print(f'| {v*100.0/n:.1f}% | {v} | {k} |')


def tls(samples, label, top=5):
    cons = collections.Counter()
    hits = 0
    for _, leaf, chain, frames in samples:
        names = [short(leaf)] + [short(c) for c in chain]
        for i, nm in enumerate(names):
            if 'LocalKey' in nm:
                hits += 1
                cons[names[i + 1] if i + 1 < len(names) else '<chain ends: indirect caller>'] += 1
                break
    print(f'\n**LocalKey::with, {label}: on the proven chain in {hits} samples '
          f'= {hits*100.0/max(len(samples),1):.1f}% of the window**\n')
    print('| % of TLS samples | n | consumer |')
    print('|---:|---:|---|')
    for k, v in cons.most_common(top):
        print(f'| {v*100.0/max(hits,1):.1f}% | {v} | `{k}` |')


if __name__ == '__main__':
    if len(sys.argv) < 3:
        print(__doc__)
        print('usage: RAVA_PC_TRACE_DIR=<dir> python pc_trace_report.py <tag> <ring_start_epoch_ms>')
        print('       ... <tag> --ring   finds the residue->ring edge')
        raise SystemExit(2)
    tag = sys.argv[1]
    if sys.argv[2] == '--ring':
        # The residue->ring edge is not a log line: it is the first sample after
        # `Starting minecraft server version` whose stack names the ring's own work.
        tr, mk = load(tag), log_marks(tag)
        for r in tr:
            if r[0] >= mk['start'] and any(
                    k in (r[1] + ' ' + ' '.join(r[2]) + ' ' + ' '.join(r[3]))
                    for k in ('findBiome', 'Climate')):
                print('ring starts at epoch_ms %d  (+%.2fs)' % (r[0], (r[0] - mk['start']) / 1000.0))
                break
        raise SystemExit(0)
    ring = int(sys.argv[2])
    tr = load(tag)
    mk = log_marks(tag)
    W = [('(a) BLOCKS A+B — process start to `Environment:`', tr[0][0], mk['env']),
         ('(b) BLOCK C — `Environment:` to `Starting minecraft server version`', mk['env'], mk['start']),
         ('(c) SERIAL RESIDUE — `Starting minecraft server version` to first ring task', mk['start'], ring),
         ('(d) RING — first ring task to `Done`', ring, mk['done'])]
    for nm, lo, hi in W:
        s = [r for r in tr if lo <= r[0] < hi]
        report(f'{nm}  [{(hi-lo)/1000.0:.1f}s wall]', s)
        tls(s, nm.split('—')[0].strip())
