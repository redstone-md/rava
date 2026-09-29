//! Launcher for the parity core.
//!
//! `rava-core [-cp PATH] [-Dk=v]... MAINCLASS [ARGS]...` boots the JDK and runs `main`.
//! `rava-core -cp DIR --call CLASS METHOD` runs a static no-argument method on the bare VM
//! (no JDK initialization) and prints its result — a tracer for the execution core.

use rava_core::natives::describe_throwable;
use rava_core::symbol::Symbol;
use rava_core::thread::Thread;
use rava_core::vm::{Vm, VmOptions};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let mut options = VmOptions::default();
    let mut call = false;
    while let Some(a) = args.peek().cloned() {
        if a == "-cp" || a == "-classpath" || a == "--class-path" {
            args.next();
            let cp = args.next().unwrap_or_default();
            options.boot_append.extend(std::env::split_paths(&cp).filter(|p| p.is_dir()));
            options.class_path = cp;
        } else if let Some(kv) = a.strip_prefix("-D") {
            args.next();
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            options.properties.push((k.into(), v.into()));
        } else if a == "--call" {
            args.next();
            call = true;
        } else {
            break;
        }
    }
    let rest: Vec<String> = args.collect();
    if rest.is_empty() {
        eprintln!("usage: rava-core [-cp PATH] [-Dk=v] MAINCLASS [ARGS] | -cp DIR --call CLASS METHOD");
        return ExitCode::from(2);
    }
    if !call {
        // Application classes come from the system class loader, not the boot path.
        options.boot_append.clear();
    }
    let worker = std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(move || if call { run_call(options, &rest) } else { run_main(options, &rest) })
        .expect("spawn main thread");
    worker.join().unwrap_or(ExitCode::from(70))
}

fn run_main(options: VmOptions, rest: &[String]) -> ExitCode {
    let started = std::time::Instant::now();
    let vm = Vm::create(options).expect("create VM");
    let mut t = Thread::new();
    let result = rava_core::boot::boot(&mut t).and_then(|_| {
        if std::env::var_os("RAVA_CORE_TIMING").is_some() {
            eprintln!("[rava-core] JDK booted in {:.1} ms, {} classes", started.elapsed().as_secs_f64() * 1e3, vm.loaded_class_count());
        }
        rava_core::boot::run_main(&mut t, &rest[0], &rest[1..])
    });
    report(result.map(|_| 0), started, vm)
}

fn run_call(options: VmOptions, rest: &[String]) -> ExitCode {
    let [class, method] = rest else {
        eprintln!("--call needs CLASS METHOD");
        return ExitCode::from(2);
    };
    let started = std::time::Instant::now();
    let vm = Vm::create(options).expect("create VM");
    let mut t = Thread::new();
    let k = match vm.load_boot_class(Symbol::intern(&class.replace('.', "/"))) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{}: {}", e.kind, e.message);
            return ExitCode::from(1);
        }
    };
    let Some(m) = k.methods().iter().find(|m| m.name == method.as_str() && m.is_static() && m.desc.as_str().starts_with("()")) else {
        eprintln!("no static no-arg method {method} in {class}");
        return ExitCode::from(1);
    };
    let result = rava_core::runtime::initialize(&mut t, k).and_then(|_| t.call(m, &[]));
    if let Ok(v) = result {
        match m.sig.ret {
            rava_core::types::BasicType::Long => println!("{}", v as i64),
            rava_core::types::BasicType::Void => {}
            _ => println!("{}", v as i32),
        }
    }
    report(result, started, vm)
}

fn report(result: Result<u64, rava_core::thread::Throw>, started: std::time::Instant, vm: &Vm) -> ExitCode {
    if let Some(s) = rava_core::jit::stats::get() {
        s.report();
    }
    if std::env::var_os("RAVA_CORE_TIMING").is_some() {
        eprintln!(
            "[rava-core] {:.1} ms, {} classes, {} KiB heap",
            started.elapsed().as_secs_f64() * 1e3,
            vm.loaded_class_count(),
            vm.heap.used_bytes() / 1024
        );
    }
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(ex) => {
            eprint!("Exception in thread \"main\" {}", describe_throwable(ex.0));
            ExitCode::from(1)
        }
    }
}
