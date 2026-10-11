#!/usr/bin/env python3
"""Server tick throughput A/B: `/tick sprint` on a fresh world.

    bench/tick_sprint.py [-n ROUNDS] [-s SPRINT] [-r SPRINTS] -b BASE JAVA_OR_RAVA_BINARY ...

BASE is a server directory set up once (as for paper_ab.sh: a `launch.args` file with the
server's arguments, or a paper jar). Each run copies it, deletes the world, starts the server
with the binary, waits for "Done (" and 20 s more, sends `tick sprint SPRINT` (default `1d`,
24000 ticks) on the console and reports the ticks per second the server reached, which is how
fast its single game thread runs flat out. With -r, each server runs that many sprints in a row
(later ones show the speed once compilation has caught up). Binaries run in the given order
each round.

With -c SCENARIO (a file in bench/scenarios/), its console commands build a load before the
sprints: one command per line, `#` comments, `#wait N` pauses N seconds, and
`#for VAR FROM TO STEP COMMAND` repeats COMMAND with `{VAR}` replaced by FROM, FROM+STEP, ...
up to TO inclusive, and `#include FILE` reads another scenario file of the same directory.
"""
import argparse, os, re, shutil, subprocess, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument('-n', type=int, default=2)
ap.add_argument('-s', default='1d')
ap.add_argument('-r', type=int, default=1)
ap.add_argument('-b', required=True)
ap.add_argument('-c')
ap.add_argument('bins', nargs='+')
a = ap.parse_args()


def scenario(path):
    """The console commands of a scenario file, with `('wait', seconds)` for pauses."""
    out = []
    for line in open(path):
        line = line.strip()
        if line.startswith('#include '):
            out += scenario(os.path.join(os.path.dirname(path), line.split()[1]))
        elif line.startswith('#wait '):
            out.append(('wait', float(line.split()[1])))
        elif line.startswith('#for '):
            _, var, lo, hi, step, cmd = line.split(None, 5)
            out += [cmd.replace('{' + var + '}', str(v)) for v in range(int(lo), int(hi) + 1, int(step))]
        elif line and not line.startswith('#'):
            out.append(line)
    return out


def run(binary):
    d = tempfile.mkdtemp(prefix='tick-sprint-')
    try:
        subprocess.run(['cp', '-r', a.b + '/.', d], check=True)
        for x in ['world', 'logs', 'crash-reports']:
            shutil.rmtree(os.path.join(d, x), ignore_errors=True)
        os.mkfifo(d + '/in.fifo')
        if os.path.exists(d + '/launch.args'):
            args = open(d + '/launch.args').read().split()
        else:
            jar = next(f for f in os.listdir(d) if f.startswith('paper-') and f.endswith('.jar'))
            args = ['-jar', jar, '--nogui']
        p = subprocess.Popen(f'exec {binary} {" ".join(args)} < in.fifo > out.txt 2>&1', shell=True, cwd=d)
        fifo = open(d + '/in.fifo', 'w')
        # The shell creates out.txt only once the FIFO is open on both ends.
        log = lambda: open(d + '/out.txt', errors='replace').read() if os.path.exists(d + '/out.txt') else ''
        t0, res = time.time(), []
        while time.time() - t0 < 900 and p.poll() is None and 'Done (' not in log():
            time.sleep(1)
        if 'Done (' in log():
            time.sleep(20)
            for c in scenario(a.c) if a.c else []:
                if isinstance(c, tuple):
                    time.sleep(c[1])
                else:
                    fifo.write(c + '\n')
                    fifo.flush()
            for _ in range(a.r):
                fifo.write(f'tick sprint {a.s}\n')
                fifo.flush()
                ts = time.time()
                while time.time() - ts < 1800 and p.poll() is None:
                    m = re.findall(r'Sprint completed with ([0-9.]+) ticks per second, or ([0-9.]+) ms per tick', log())
                    if len(m) > len(res):
                        res.append(f'{m[-1][0]} tps {m[-1][1]} ms/tick')
                        break
                    time.sleep(1)
        res = ', '.join(res) or 'FAILED'
        try:
            fifo.write('stop\n')
            fifo.flush()
        except BrokenPipeError:
            pass
        try:
            p.wait(timeout=180)
        except subprocess.TimeoutExpired:
            p.kill()
        return res
    finally:
        shutil.rmtree(d, ignore_errors=True)


for r in range(1, a.n + 1):
    for b in a.bins:
        print(f'round {r} {b}: {run(b)}', flush=True)
