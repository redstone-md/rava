//! `rava` — a generic JVM bytecode engine, invoked like `java`.
//!
//! Drop-in command line (so users only swap `java` → `rava`):
//!
//!     rava [options] -jar <file.jar> [args...]
//!     rava [options] [-cp <path>] <MainClass> [args...]
//!
//! Recognised options: `-jar`, `-cp`/`-classpath`/`--class-path`, `-Xmx<size>`
//! / `-Xms<size>`, `-D<key>=<value>`. Other JVM flags (`-XX:…`, `-ea`,
//! `-verbose…`, `-server`, …) are accepted and ignored for compatibility.
//!
//! Tunables resolve in this order: command line → environment → `rava.toml` →
//! built-in default. A `rava.toml` is created on first run recording the
//! detected core count and the memory cap (see [`Config`]).
//!
//! Legacy `rava jar <path>` / `rava run <Class.class>` still work.



use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

mod run;
use run::*;
mod config;
use config::*;
mod proc_cap;
#[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
mod prepare_aot;

// mimalloc returns freed pages to the OS (the system allocator retained ~20GB of
// post-GC garbage as RSS); keeps resident memory near the live set. Wrapped in a
// counting allocator that feeds `rava_interp::mem_meter` the live byte total, so
// the GC scheduler triggers on REAL process memory rather than the heap's byte
// estimate (which undercounts true RSS ~5x and let the heap balloon before GC).
#[global_allocator]
static GLOBAL: Counting = Counting(mimalloc::MiMalloc);

struct Counting(mimalloc::MiMalloc);

unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = self.0.alloc(l);
        if !p.is_null() {
            rava_interp::mem_meter::add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        self.0.dealloc(p, l);
        rava_interp::mem_meter::sub(l.size());
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = self.0.alloc_zeroed(l);
        if !p.is_null() {
            rava_interp::mem_meter::add(l.size());
        }
        p
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, new: usize) -> *mut u8 {
        let np = self.0.realloc(p, l, new);
        if !np.is_null() {
            rava_interp::mem_meter::sub(l.size());
            rava_interp::mem_meter::add(new);
        }
        np
    }
}

use rava_classfile::ClassFile;
use rava_interp::{ClassLoader, Vm};
use rava_jar::Jar;

const MAIN_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// PGO TRAINING ONLY. Writes the instrumentation counters and exits after
/// `RAVA_PGO_DUMP_AFTER_MS`, so a workload with NO CLEAN EXIT can still be trained on.
///
/// This exists because of a specific gap: the profile that ships is trained on
/// `BootPhaseC` + `ChunkFill`, chosen only because both return from `main` — the real
/// server boot never does (`docs/pgo-build.md` trap 2: writing `stop` to its stdin does
/// nothing, and LLVM continuous mode produces a profile `llvm-profdata` refuses as
/// truncated). So the shipped binary is optimised for a PROXY of the workload it is
/// measured on. Arming a timer that calls `__llvm_profile_write_file` closes that gap
/// without needing the guest to cooperate.
///
/// `_exit` is deliberate: the counters are already on disk, and running Drop for a live
/// server would be both slow and pointless. Compiled in only under the `pgo-dump` feature
/// — the symbol does not exist outside a `-C profile-generate` build.
#[cfg(feature = "pgo-dump")]
fn pgo_dump_arm() {
    let Some(ms) = std::env::var("RAVA_PGO_DUMP_AFTER_MS").ok().and_then(|v| v.trim().parse::<u64>().ok())
    else {
        return;
    };
    extern "C" {
        fn __llvm_profile_write_file() -> std::os::raw::c_int;
    }
    std::thread::Builder::new()
        .name("rava-pgo-dump".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            // SAFETY: provided by the profile runtime that `-C profile-generate` links.
            let rc = unsafe { __llvm_profile_write_file() };
            eprintln!("[PGO-DUMP] wrote counters after {ms} ms (rc={rc}), exiting");
            std::process::exit(0);
        })
        .expect("spawn rava-pgo-dump");
}

#[cfg(not(feature = "pgo-dump"))]
fn pgo_dump_arm() {}

fn main() -> ExitCode {
    // Must be the FIRST thing: these flags are read from inside the global allocator, so they
    // cannot read the environment themselves (see `mem_meter::BATCH_OFF`).
    rava_interp::mem_meter::init_from_env();
    rava_interp::start_census_timer_if_on();
    rava_interp::diag::register_builtin_gauges();
    rava_interp::diag::start_timers_if_on();
    pgo_dump_arm();
    // Return freed pages to the OS promptly instead of mimalloc's lazy decay —
    // under worldgen's heavy short-lived allocation the default retained ~7GB of
    // already-freed pages as RSS. Purge delay 0 keeps resident memory tracking the
    // live set (~1GB vs ~7GB). Set before the engine thread does its allocation.
    if std::env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
        std::env::set_var("MIMALLOC_PURGE_DELAY", "0");
    }
    // Deep Java call trees map onto deep native recursion, so run the engine on
    // a thread with a large stack (mirroring a JVM's -Xss).
    std::thread::Builder::new()
        .stack_size(1 << 30) // 1 GiB
        .spawn(dispatch)
        .expect("spawn engine thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: rava [options] -jar <file.jar> [args...]\n\
         \x20      rava [options] [-cp <path>] <MainClass> [args...]\n\
         options: -jar -cp/-classpath -Xmx<size> -Xms<size> -D<k>=<v> (other JVM flags ignored)"
    );
    ExitCode::FAILURE
}

// --- command line -----------------------------------------------------------

enum Launch {
    Jar(String, Vec<String>),
    Class(String, Vec<String>),
    None,
}

struct Cli {
    launch: Launch,
    classpath: Vec<PathBuf>,
    props: HashMap<String, String>,
    memory_mb: Option<u64>,
}

/// Parse a JVM memory size (`4G`, `2048m`, `512k`, or bare bytes) to mebibytes.
fn parse_size_mb(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let (num, mult) = match s.chars().last().unwrap().to_ascii_lowercase() {
        'g' => (&s[..s.len() - 1], 1024),
        'm' => (&s[..s.len() - 1], 1),
        'k' => (&s[..s.len() - 1], 0), // < 1 MiB rounds down; keep at least 1 below
        _ => (s, 0),                    // bare bytes
    };
    let n: u64 = num.trim().parse().ok()?;
    Some(match mult { 1024 => n * 1024, 1 => n, _ => (n / (1024 * 1024)).max(1) })
}

// --- config file ------------------------------------------------------------

/// Parse a config/env value as a boolean: `0/false/no/off` → false, else true.
fn cfg_bool(s: &str) -> bool {
    !matches!(s.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off" | "")
}

/// Export every raw `RAVA_*` key from the config file into the process environment, UNLESS the
/// variable is already set in the real environment (so a shell var / CLI flag still wins). This is
/// what makes a single `rava.toml` able to configure ALL of RAVA's env-var tunables — no need to
/// remember or pass a wall of `RAVA_*=…` on the command line. Friendly aliases (memory_mb, jit,
/// jit_cap, jit_threshold, gc, threads) are resolved separately by `Config::resolve` and take
/// precedence for the handful of settings they cover.
fn apply_raw_flags(file: &HashMap<String, String>) {
    for (k, v) in file {
        if k.starts_with("RAVA_") && std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

/// ── Human-readable config aliases ────────────────────────────────────────────
/// The engine grew ~230 `RAVA_*` env switches with cryptic grown-not-designed names.
/// This table maps CLEAN sectioned config names onto them, so `rava.toml` reads like a
/// real config while the code keeps its historical env vars (no 200-call-site rename):
///   • `Val`  — the value passes through verbatim (numbers, strings).
///   • `Flag` — default-OFF feature: a truthy value sets the env to "1"; falsy = leave off.
///   • `Inv`  — default-ON feature behind a `RAVA_NO_*` kill-switch: a FALSY value sets
///              the kill-switch; truthy = leave on. So `jit.ic_deep = false` reads sanely
///              instead of the double-negative `RAVA_NO_IC_DEEP = 1`.
/// Shell env always wins (only-if-unset export, same rule as raw keys). Legacy `RAVA_*`
/// keys in the file keep working — deep forensics flags stay usable un-aliased.
enum AliasKind { Val, Flag, Inv }
fn truthy(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes")
}

fn verbose() -> bool {
    std::env::var("RAVA_VERBOSE").is_ok()
}

fn finish(result: Result<(), Box<dyn std::error::Error>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rava: {e}");
            ExitCode::FAILURE
        }
    }
}
