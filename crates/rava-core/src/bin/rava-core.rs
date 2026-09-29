//! Tracer launcher for the parity core.
//!
//! `rava-core [-cp DIR]... CLASS METHOD` calls the static no-argument method `METHOD` of `CLASS`
//! (returning `I`, `J`, `Z` or `V`) and prints its result. Full `main(String[])` launching arrives
//! with `System.initPhase1`.

use rava_core::natives::describe_throwable;
use rava_core::symbol::Symbol;
use rava_core::thread::Thread;
use rava_core::vm::{Vm, VmOptions};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut options = VmOptions::default();
    let mut positional = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-cp" | "-classpath" => options.boot_append.extend(std::env::split_paths(&args.next().unwrap_or_default())),
            _ => positional.push(a),
        }
    }
    let [class, method] = positional.as_slice() else {
        eprintln!("usage: rava-core [-cp DIR] CLASS METHOD");
        return ExitCode::from(2);
    };
    let (class, method) = (class.replace('.', "/"), method.clone());
    let worker = std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(move || run(options, &class, &method))
        .expect("spawn main thread");
    worker.join().unwrap_or(ExitCode::from(70))
}

fn run(options: VmOptions, class: &str, method: &str) -> ExitCode {
    let started = std::time::Instant::now();
    let vm = Vm::create(options).expect("create VM");
    let mut t = Thread::new();
    let k = match vm.load_boot_class(Symbol::intern(class)) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{}: {}", e.kind, e.message);
            return ExitCode::from(1);
        }
    };
    let Some(m) = k.methods().iter().find(|m| m.name == method && m.is_static() && m.desc.as_str().starts_with("()")) else {
        eprintln!("no static no-arg method {method} in {class}");
        return ExitCode::from(1);
    };
    let result = rava_core::runtime::initialize(&mut t, k).and_then(|_| t.call(m, &[]));
    let elapsed = started.elapsed();
    match result {
        Ok(v) => {
            match m.sig.ret {
                rava_core::types::BasicType::Long => println!("{}", v as i64),
                rava_core::types::BasicType::Void => {}
                _ => println!("{}", v as i32),
            }
            eprintln!(
                "[rava-core] {:.1} ms, {} classes, {} KiB heap",
                elapsed.as_secs_f64() * 1e3,
                vm.loaded_class_count(),
                vm.heap.used_bytes() / 1024
            );
            ExitCode::SUCCESS
        }
        Err(ex) => {
            eprint!("Exception in thread \"main\" {}", describe_throwable(ex.0));
            ExitCode::from(1)
        }
    }
}
