//! Running a class, a jar, or a classpath.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::*;

pub(crate) fn dispatch() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
    if argv.first().is_some_and(|arg| arg == "--prepare-aot") {
        return crate::prepare_aot::run(&argv[1..]);
    }
    #[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
    let _ = rava_interp::prepared::directory();
    // Legacy subcommands.
    match argv.first().map(String::as_str) {
        Some("jar") => return match argv.get(1) { Some(p) => run_jar(p, &argv[2..]), None => usage() },
        Some("run") => return match argv.get(1) { Some(p) => run_class(p, &argv[2..]), None => usage() },
        Some("-version" | "--version" | "-showversion") => {
            println!("rava 0.1.0 (generic JVM bytecode engine)");
            return ExitCode::SUCCESS;
        }
        Some("-h" | "-help" | "--help" | "-?") | None => return usage(),
        _ => {}
    }

    let cli = match parse(&argv) {
        Ok(c) => c,
        Err(e) => { eprintln!("rava: {e}"); return usage(); }
    };
    // Costs nothing until something faults; turns a silent 0xC0000005 into a named line.
    rava_interp::crash_handler::install();
    let cfg = Config::resolve(cli.memory_mb);
    cfg.apply();
    // Arm the real-memory safety ceiling (see rava_interp::mem_meter). The soft
    // -Xmx cap and GC only act at bytecode checkpoints, so a runaway native
    // recursion on a cyclic graph is invisible to them — this allocator-level
    // ceiling is what actually stops it from exhausting host RAM. Defaults to 4x
    // the soft heap cap, floored at 2 GiB: above any legitimate working set, far
    // below the ~31 GB a runaway once reached. Override with RAVA_HARD_MB.
    let hard_mb = std::env::var("RAVA_HARD_MB").ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or_else(|| cfg.memory_mb.saturating_mul(4).max(2048));
    rava_interp::mem_meter::set_hard_limit((hard_mb as usize).saturating_mul(1024 * 1024));
    if std::env::var("RAVA_VERBOSE").is_ok() {
        eprintln!("rava: hard real-memory ceiling {hard_mb} MiB (RAVA_HARD_MB)");
    }
    // OS-ENFORCED process cap (Windows Job Object): the kernel refuses any commit
    // past this ceiling, so a leak the metered allocator can't see still cannot take
    // down the host. The true safety backstop for danger-zone work. RAVA_PROC_CAP_MB.
    if let Some(cap_mb) = std::env::var("RAVA_PROC_CAP_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        if proc_cap::apply(cap_mb) {
            eprintln!("rava: OS process memory cap {cap_mb} MiB (RAVA_PROC_CAP_MB) — kernel-enforced");
        } else {
            eprintln!("rava: WARNING — failed to apply RAVA_PROC_CAP_MB={cap_mb} (NOT protected by an OS cap)");
        }
    }
    rava_interp::set_user_properties(cli.props);

    match cli.launch {
        Launch::Jar(path, args) => run_jar(&path, &args),
        Launch::Class(name, args) => run_classpath(&name, &cli.classpath, &args),
        Launch::None => usage(),
    }
}

/// Execute `main(String[])` of a single `.class` file on disk.
pub(crate) fn run_class(path: &str, args: &[String]) -> ExitCode {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let bytes = std::fs::read(path)?;
        let class = ClassFile::parse(&bytes)?;
        let main_class = class.this_class.clone();
        let dir = std::path::Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let loader = ClassLoader::new().with_dir(dir);
        let mut vm = Vm::new(loader);
        let run = vm.run_main_with_args(&main_class, args);
        if let Err(e) = &run {
            if let Some(d) = vm.describe_error(e) {
                eprintln!("rava: uncaught {d}");
            }
        }
        vm.dump_jit_stats();
        rava_interp::prof::dump();
        rava_interp::pool_scan_dump_if_on();
        run?;
        Ok(())
    })();
    finish(result)
}

/// Run `MainClass` resolved from a classpath of dirs/jars.
pub(crate) fn run_classpath(name: &str, classpath: &[PathBuf], args: &[String]) -> ExitCode {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let st = std::env::var_os("RAVA_STARTUP_TIME").is_some();
        let t0 = std::time::Instant::now();
        let mut loader = ClassLoader::new();
        for p in classpath {
            if p.extension().map(|e| e == "jar").unwrap_or(false) {
                loader = loader.with_jar(Jar::open(p)?);
            } else {
                loader = loader.with_dir(p.clone());
            }
        }
        if classpath.is_empty() {
            loader = loader.with_dir(std::env::current_dir()?);
        }
        let t_loader = t0.elapsed();
        let mut vm = Vm::new(loader);
        let t_vmnew = t0.elapsed();
        let run = vm.run_main_with_args(name, args);
        if st {
            let t_run = t0.elapsed();
            eprintln!(
                "[STARTUP] loader={:.1}ms vm_new={:.1}ms run_main={:.1}ms total={:.1}ms",
                t_loader.as_secs_f64() * 1e3,
                (t_vmnew - t_loader).as_secs_f64() * 1e3,
                (t_run - t_vmnew).as_secs_f64() * 1e3,
                t_run.as_secs_f64() * 1e3,
            );
        }
        if let Err(e) = &run {
            if let Some(d) = vm.describe_error(e) {
                eprintln!("rava: uncaught {d}");
            }
        }
        vm.dump_jit_stats();
        rava_interp::prof::dump();
        rava_interp::pool_scan_dump_if_on();
        if std::env::var_os("RAVA_HEAP_DUMP").is_some() {
            eprint!("{}", vm.heap.composition());
        }
        if rava_interp::devirt::enabled() {
            rava_interp::devirt::dump();
        }
        if rava_interp::devirt::enabled() || rava_interp::devirt::steady_enabled() {
            rava_interp::devirt::dump_llvm_eligibility();
        }
        if rava_interp::devirt::steady_enabled() {
            rava_interp::devirt::dump_steady();
        }
        if rava_interp::m3prof::enabled() {
            rava_interp::m3prof::dump();
        }
        if rava_interp::m3shape::enabled() {
            rava_interp::m3shape::dump();
        }
        if rava_interp::m3engine::enabled() {
            rava_interp::m3engine::dump();
        }
        if rava_interp::m3heap::probe_on() {
            rava_interp::m3heap::dump();
        }
        if rava_interp::m3biome::enabled() {
            rava_interp::m3biome::dump();
        }
        if rava_interp::bootdisp::on() {
            rava_interp::bootdisp::dump();
        }
        if rava_interp::icmiss::on() {
            rava_interp::icmiss::dump();
        }
        rava_interp::datapack_cache::dump_if_on();
        rava_interp::residue_probe::dump();
        rava_interp::rava_sampler::dump();
        rava_interp::pc_profile::dump::dump("exit");
        rava_interp::opdump::dump_if_on();
        {
            let fs = rava_interp::field_site_census::report();
            if !fs.is_empty() {
                eprint!("{fs}");
            }
        }
        {
            let iv = rava_interp::invoke_site_census::report();
            if !iv.is_empty() {
                eprint!("{iv}");
            }
        }
        {
            let pc = rava_interp::pair_census::report();
            if !pc.is_empty() {
                eprint!("{pc}");
            }
        }
        {
            let bce = rava_interp::bce_census::report();
            if !bce.is_empty() {
                eprint!("{bce}");
            }
        }
        {
            let ac = rava_interp::arraycopy_census::report();
            if !ac.is_empty() { eprint!("{ac}"); }
        }
        {
            let rc = rava_interp::resolve_census::report();
            if !rc.is_empty() { eprint!("{rc}"); }
        }
        {
            let sh = rava_interp::jit_shape_census::report();
            if !sh.is_empty() { eprint!("{sh}"); }
        }
        rava_interp::diag::dump("main-return");
        rava_interp::interp_who::dump(&vm);
        rava_interp::chain_census_report();
        rava_interp::seam_census_report();
        {
            let cr = rava_interp::hot_cold_report();
            if !cr.is_empty() {
                eprintln!("{cr}");
            }
            let hc = rava_interp::hot_census_report();
            if !hc.is_empty() {
                eprintln!("{hc}");
            }
            let pc = rava_interp::pool_census_report();
            if !pc.is_empty() {
                eprintln!("{pc}");
            }
            let sc = rava_interp::shape_census::report();
            if !sc.is_empty() {
                eprintln!("{sc}");
            }
            let ho = rava_interp::gil::handoff_report();
            if !ho.is_empty() {
                eprintln!("{ho}");
            }
            let sl = rava_interp::slot_census::report();
            if !sl.is_empty() {
                eprintln!("{sl}");
            }
            let fc = rava_interp::flatcall::report();
            if !fc.is_empty() {
                eprintln!("{fc}");
            }
            let fb = rava_interp::flatcall::blockers_report();
            if !fb.is_empty() {
                eprintln!("{fb}");
            }
        }
        rava_interp::dispatch_gate_dump();
        rava_interp::codec_flatten::dump();
        rava_interp::recordast::dump();
        rava_interp::jitsig::dump();
        rava_interp::pure_memo::dump();
        rava_interp::picprobe::dump();
        rava_interp::refpot::dump();
        rava_interp::directprobe::dump();
        rava_interp::selfreg::dump();
        rava_interp::directmono::dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::heappic::dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::shadow::dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::shadow::flat_dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::shadow::arm_dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::fork_scan::dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::structkey_scan::dump();
        #[cfg(feature = "next-gen-gc")]
        rava_interp::escape_scan::dump();
        // Final cumulative collector line (RAVA_GC_STATS). A bench exits cleanly, so
        // unlike a server boot it can report the totals rather than a trajectory.
        rava_interp::gc_stats_dump();
        // COLD-START FAST-EXIT: the diagnostic dumps above are no-ops when their
        // flags are off; the remaining teardown (Vm/heap/JIT Drop after this
        // closure) is pure latency the OS makes redundant — it unmaps the arena,
        // frees the handle table and JIT modules on process exit anyway. For a
        // short program that teardown is a large fraction of wall time. Flush
        // guest stdout/stderr (so nothing is lost), then exit directly, skipping
        // the Drop. Opt out with RAVA_NO_FAST_EXIT (e.g. for leak checking).
        if std::env::var_os("RAVA_NO_FAST_EXIT").is_none() {
            use std::io::Write as _;
            // ⛔SAY WHY BEFORE LEAVING. This exited on `run.is_err()` and DISCARDED the
            // error, so every engine-side failure on this path died mute: exit code 1,
            // not one word on stderr. A missing static (`getstatic
            // java/lang/String.CASE_INSENSITIVE_ORDER`) looked exactly like a program
            // that had simply stopped, and the fast exit below is the reason the usual
            // `rava: {e}` never ran.
            if let Err(e) = &run {
                eprintln!("rava: {e}");
            }
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(if run.is_err() { 1 } else { 0 });
        }
        run?;
        Ok(())
    })();
    finish(result)
}

/// Run a jar's `Main-Class`, java `-jar` style.
pub(crate) fn run_jar(path: &str, args: &[String]) -> ExitCode {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut jar = Jar::open(path)?;
        let main_class = jar.main_class()?;
        if verbose() {
            let bytes = jar.read_class(&main_class)?
                .ok_or_else(|| format!("main class {main_class} not present in jar"))?;
            let class = ClassFile::parse(&bytes)?;
            let java = rava_classfile::version::java_feature(class.major);
            eprintln!("rava: {path} → {main_class} (Java {java}, {} classes)", jar.class_count());
        }
        let loader = ClassLoader::new().with_jar(Jar::open(path)?);
        let mut vm = Vm::new(loader);
        let run = vm.run_main_with_args(&main_class, args);
        if let Err(e) = &run {
            if let Some(d) = vm.describe_error(e) {
                eprintln!("rava: uncaught {d}");
            }
        }
        vm.dump_jit_stats();
        rava_interp::prof::dump();
        run?;
        Ok(())
    })();
    finish(result)
}
