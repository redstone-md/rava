#!/usr/bin/env python3
"""Preserved-log, isolated-process A/B with an external per-tick Paper observer."""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import time


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def scenario(path, parents=()):
    path = path.resolve()
    if path in parents:
        raise ValueError("Recursive scenario include")
    commands = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if line.startswith("#include "):
            commands += scenario(path.parent / line.split()[1], (*parents, path))
        elif line.startswith("#wait "):
            seconds = float(line.split()[1])
            if seconds < 0:
                raise ValueError("Negative scenario wait")
            commands.append(["wait", seconds])
        elif line.startswith("#for "):
            _, var, lo, hi, step, command = line.split(None, 5)
            lo, hi, step = int(lo), int(hi), int(step)
            commands += [command.replace("{" + var + "}", str(i))
                         for i in range(lo, hi + (1 if step > 0 else -1), step)]
        elif line and not line.startswith("#"):
            commands.append(line)
    return commands


def config_hashes(base):
    # World saves, logs and native profiler files are not benchmark configuration.
    return {str(p.relative_to(base)): digest(p) for p in sorted(base.rglob("*"))
            if p.is_file() and p.suffix in (".yml", ".yaml", ".properties")
            and not any(x.startswith("world") or x in ("logs", "crash-reports") for x in p.relative_to(base).parts)}


def run(args, label, binary, expected_hash, ordinal, commands, server_args, cpus):
    result_dir = args.out / f"run-{ordinal:02d}-{label}"
    result_dir.mkdir()
    work = result_dir / "server"
    log_path = result_dir / "server.log"
    result = {"label": label, "ordinal": ordinal, "binary_sha256": expected_hash,
              "phases": [], "commands": [], "success": False, "killed": False, "stop_requested": False}
    process = None
    started = time.monotonic()

    def log():
        return log_path.read_text(errors="replace") if log_path.exists() else ""

    def send(command):
        if process.poll() is not None:
            raise RuntimeError("Server exited before command")
        process.stdin.write(command + "\n")
        process.stdin.flush()
        result["commands"].append({"command": command, "seconds": time.monotonic() - started})

    def wait_for(check, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text = log()
            if "TICKMEASURE_ERROR " in text:
                raise RuntimeError("Observer error; see preserved server.log")
            value = check(text)
            if value:
                return value
            if process.poll() is not None:
                raise RuntimeError("Server exited before expected output")
            time.sleep(.25)
        raise TimeoutError("Timed out waiting for server output")

    try:
        if digest(binary) != expected_hash:
            raise RuntimeError("Binary changed after manifest creation")
        with socket.socket() as port:
            port.bind(("127.0.0.1", 25599))
        shutil.copytree(args.base, work)
        for name in (args.world_name, args.world_name + "_nether", args.world_name + "_the_end", "logs", "crash-reports"):
            shutil.rmtree(work / name, ignore_errors=True)
        plugin_dir = work / "plugins"
        plugin_dir.mkdir(exist_ok=True)
        shutil.rmtree(plugin_dir / "TickMeasure", ignore_errors=True)
        shutil.copy2(args.plugin, plugin_dir / "TickMeasure.jar")
        result["argv"] = [str(binary), *server_args]
        affinity = (lambda: os.sched_setaffinity(0, cpus)) if cpus else None
        with log_path.open("wb") as output:
            process = subprocess.Popen(result["argv"], cwd=work, stdin=subprocess.PIPE,
                                       stdout=output, stderr=subprocess.STDOUT, text=True, preexec_fn=affinity)
            result["pid"] = process.pid
            wait_for(lambda text: "Done (" in text, args.timeout)
            time.sleep(args.settle_seconds)
            for command in commands:
                if isinstance(command, list):
                    time.sleep(command[1])
                else:
                    send(command)
            expected_summons = sum(isinstance(c, str) and c.startswith("summon ") for c in commands)
            if expected_summons:
                wait_for(lambda text: text.count("Summoned new") >= expected_summons, args.timeout)
            for phase_no in range(1, args.phases + 1):
                phase = f"phase-{phase_no:02d}"
                send(f"tickmeasure start {phase} {args.warmup_ticks} {args.ticks} {args.mode}")
                wait_for(lambda text: f"TICKMEASURE_ARMED {phase}" in text, args.timeout)
                def captured(text):
                    for raw in re.findall(r"TICKMEASURE_RESULT (\{[^\n]+\})", text):
                        value = json.loads(raw)
                        if value["phase"] == phase:
                            return value
                value = wait_for(captured, args.timeout)
                if (value["measure_ticks"] != args.ticks or value["warmup_ticks"] != args.warmup_ticks
                        or value["tick_gaps"] or value["invalid_durations"] or value["mode"] != args.mode
                        or value["sprint_ticks"] != (args.ticks if args.mode == "sprint" else 0)):
                    raise RuntimeError("Invalid or incomplete captured window")
                raw = work / "plugins/TickMeasure" / (phase + ".csv")
                with raw.open() as source:
                    rows = list(csv.DictReader(source))
                ticks = [int(row["tick"]) for row in rows]
                durations = [float(row["duration_ms"]) for row in rows]
                if (len(rows) != args.ticks or any(not math.isfinite(d) or d < 0 for d in durations)
                        or any(row["sprinting"] != str(args.mode == "sprint").lower() for row in rows)
                        or any(b != a + 1 for a, b in zip(ticks, ticks[1:]))
                        or ticks[0] != value["first_tick"] or ticks[-1] != value["last_tick"]):
                    raise RuntimeError("Raw tick CSV does not match completed capture")
                ordered = sorted(durations)
                for name, fraction in (("p50_ms", .5), ("p95_ms", .95), ("p99_ms", .99)):
                    if abs(ordered[math.ceil(fraction * len(rows)) - 1] - value[name]) > 2e-9:
                        raise RuntimeError("Reported quantile does not match raw tick CSV")
                value["csv_sha256"] = digest(raw)
                result["phases"].append(value)
                print(json.dumps({"run": ordinal, "label": label, **value}), flush=True)
            result["success"] = True
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        try:
            if process is not None:
                if process.poll() is None:
                    try:
                        send("stop")
                        result["stop_requested"] = True
                    except (BrokenPipeError, OSError, RuntimeError):
                        pass
                    try:
                        process.wait(timeout=args.stop_timeout)
                    except subprocess.TimeoutExpired:
                        process.kill()  # Only our direct child, never a shared server.
                        result["killed"] = True
                        process.wait(timeout=30)
                result["returncode"] = process.returncode
                process.stdin.close()
                result["success"] &= process.returncode == 0 and not result["killed"]
        except Exception as error:
            result["success"] = False
            result["cleanup_error"] = f"{type(error).__name__}: {error}"
        result["stop_logged"] = "Stopping server" in log()
        result["success"] &= result["stop_requested"] and result["stop_logged"]
        if log_path.exists():
            result["log_sha256"] = digest(log_path)
        for source, dest in ((work / "plugins/TickMeasure", "ticks"), (work / "logs", "logs"),
                             (work / "crash-reports", "crash-reports")):
            if source.exists():
                shutil.copytree(source, result_dir / dest)
        save(result_dir / "result.json", result)
        shutil.rmtree(work, ignore_errors=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--plugin", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--vm", nargs=2, action="append", required=True, metavar=("LABEL", "ABSOLUTE_BINARY"))
    parser.add_argument("--server-args-json", type=Path, help="Trusted JSON array of exact server/JVM arguments")
    parser.add_argument("--scenario", type=Path, default=Path(__file__).parent / "scenarios/mobs.txt")
    parser.add_argument("--source-manifest", type=Path)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--phases", type=int, default=3)
    parser.add_argument("--warmup-ticks", type=int, default=1000)
    parser.add_argument("--ticks", type=int, default=3000)
    parser.add_argument("--mode", choices=("paced", "sprint"), default="paced")
    parser.add_argument("--affinity", help="Explicit allowed CPU numbers, comma-separated; requires Linux affinity")
    parser.add_argument("--seed", default="20260930")
    parser.add_argument("--settle-seconds", type=float, default=20)
    parser.add_argument("--timeout", type=float, default=900)
    parser.add_argument("--stop-timeout", type=float, default=180)
    args = parser.parse_args()
    args.base, args.plugin, args.out = args.base.resolve(), args.plugin.resolve(), args.out.resolve()
    if args.out.is_relative_to(args.base):
        parser.error("Output must be outside BASE to avoid recursive copying")
    if args.rounds < 1 or args.phases < 1 or not 0 <= args.warmup_ticks <= 2_000_000 or not 1 <= args.ticks <= 2_000_000:
        parser.error("Invalid rounds, phases or tick counts")
    if args.settle_seconds < 0 or args.timeout <= 0 or args.stop_timeout <= 0:
        parser.error("Invalid timing bounds")
    props = dict(line.split("=", 1) for line in (args.base / "server.properties").read_text().splitlines()
                 if line and not line.startswith("#") and "=" in line)
    for key, expected in (("server-port", "25599"), ("server-ip", "127.0.0.1"), ("enable-query", "false"),
                          ("enable-rcon", "false"), ("level-seed", args.seed)):
        if props.get(key) != expected:
            parser.error(f"Base must set {key}={expected}")
    args.world_name = props.get("level-name", "world")
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.world_name):
        parser.error("level-name must be a simple directory name")
    vms = []
    for label, supplied in args.vm:
        binary = Path(supplied)
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,40}", label) or any(v[0] == label for v in vms):
            parser.error("Labels must be unique portable identifiers of at most 40 characters")
        if not binary.is_absolute() or not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error("Each VM must be an executable absolute binary path")
        binary = binary.resolve()
        vms.append((label, binary, digest(binary)))
    cpus = set(map(int, args.affinity.split(","))) if args.affinity else None
    if cpus and (not hasattr(os, "sched_setaffinity") or not cpus <= os.sched_getaffinity(0)):
        parser.error("Requested CPU affinity is unavailable")
    if args.server_args_json:
        server_args = json.loads(args.server_args_json.read_text())
    else:
        jars = sorted(args.base.glob("paper-*.jar"))
        if len(jars) != 1:
            parser.error("Supply --server-args-json or exactly one paper-*.jar in BASE")
        server_args = ["-jar", jars[0].name, "--nogui"]
    if not isinstance(server_args, list) or not all(isinstance(v, str) for v in server_args):
        parser.error("Server arguments must be a JSON array of strings")
    # This Paper tracer permits JVM options followed by one prepared JAR and nogui only.
    # Argument files and server options could override the validated port/world/config paths.
    if server_args.count("-jar") != 1 or any(v.startswith(("@", "-Duser.dir=")) for v in server_args):
        parser.error("Use explicit JVM arguments, -jar, a prepared Paper JAR and --nogui only")
    jar_index = server_args.index("-jar")
    if (jar_index + 1 >= len(server_args) or "/" in server_args[jar_index + 1]
            or "\\" in server_args[jar_index + 1] or not server_args[jar_index + 1].startswith("paper-")
            or not (args.base / server_args[jar_index + 1]).is_file()
            or any(v not in ("--nogui", "nogui") for v in server_args[jar_index + 2:])):
        parser.error("Server overrides are refused; set port/world/config in the isolated BASE")
    commands = scenario(args.scenario)
    args.out.mkdir(parents=True, exist_ok=False)
    metadata = {"created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "base": str(args.base), "config_sha256": config_hashes(args.base),
                "paper_jars_sha256": {str(p.relative_to(args.base)): digest(p) for p in sorted(args.base.rglob("*.jar"))},
                "plugin_sha256": digest(args.plugin), "server_args": server_args, "scenario": str(args.scenario.resolve()),
                "expanded_commands": commands, "rounds": args.rounds, "phases": args.phases,
                "warmup_ticks": args.warmup_ticks, "measure_ticks": args.ticks, "mode": args.mode,
                "cpu_affinity": sorted(cpus) if cpus else None, "seed": args.seed, "fresh_all_dimensions": True,
                "binaries": {label: {"path": str(binary), "sha256": sha} for label, binary, sha in vms},
                "activation_caveat": "No protocol client is connected by this harness. Player-nearby full AI is not established.",
                "state_checksum": None, "simulation_caveat": "Same seed and commands do not prove identical entity simulation outcomes."}
    if args.source_manifest:
        shutil.copy2(args.source_manifest, args.out / "source-manifest.txt")
        metadata["source_manifest_sha256"] = digest(args.out / "source-manifest.txt")
    save(args.out / "manifest.json", metadata)
    results = []
    for round_no in range(args.rounds):
        for label, binary, sha in vms if round_no % 2 == 0 else reversed(vms):
            results.append(run(args, label, binary, sha, len(results) + 1, commands, server_args, cpus))
            save(args.out / "results.json", results)
            if not results[-1]["success"]:
                raise SystemExit("Measurement failed; preserved run result and logs identify the failure")


if __name__ == "__main__":
    main()
