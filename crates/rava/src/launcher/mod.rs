//! `rava` takes the `java` command line and answers as `java` does: same output, same exit codes.

mod command_line;
mod settings;
mod tokens;

use crate::boot::{self, call_static};
use crate::heap::Oop;
use crate::klass::Klass;
use crate::runtime as rt;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::vm::Vm;
use command_line::{CommandLine, LaunchError, Query};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

const HELPER: &str = "sun/launcher/LauncherHelper";
const MAIN_WITHOUT_ARGS: i32 = 1;
const MAIN_NONSTATIC: i32 = 2;

pub fn main() -> ExitCode {
    let line = match CommandLine::from_env() {
        Ok(line) => line,
        Err(e) => return fail(e),
    };
    let Some(java_home) = crate::java_home() else {
        return fail(LaunchError::Launcher("no Java class library found: rava reads <prefix>/lib/modules beside <prefix>/bin/rava (dist/make.sh builds that layout); a development build uses JAVA_HOME or the JDK of java on PATH".into()));
    };
    let library = crate::jdk::init(&java_home);
    if library.feature < crate::jdk::OLDEST {
        return fail(LaunchError::Launcher(format!(
            "the Java class library at {} is release {}; rava runs releases {} and later",
            java_home.display(),
            library.feature,
            crate::jdk::OLDEST
        )));
    }
    std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(move || run(line, java_home))
        .expect("spawn main thread")
        .join()
        .unwrap_or(ExitCode::from(70))
}

fn fail(e: LaunchError) -> ExitCode {
    e.report();
    ExitCode::FAILURE
}

/// Creates the VM, does what the command line asks, then ends the VM as `DestroyJavaVM` does.
fn run(mut line: CommandLine, java_home: PathBuf) -> ExitCode {
    let started = Instant::now();
    let jar = line.jar().map(str::to_owned);
    let options = std::mem::take(&mut line.settings).into_vm_options(java_home, jar.as_deref());
    let vm = match Vm::create(options) {
        Ok(vm) => vm,
        Err(e) => return fail(LaunchError::Vm(format!("Could not reserve space for the heap: {e}"))),
    };
    let mut t = Thread::new();
    t.attach();
    let result = boot::boot(&mut t).and_then(|_| {
        if timing() {
            eprintln!("[rava] JDK booted in {:.1} ms, {} classes", started.elapsed().as_secs_f64() * 1e3, vm.loaded_class_count());
        }
        execute(&mut t, &line)
    });
    let code = result.unwrap_or_else(|ex| {
        describe_uncaught(&mut t, "main", ex.0);
        ExitCode::FAILURE
    });
    if let Err(ex) = boot::destroy(&mut t) {
        describe_uncaught(&mut t, "DestroyJavaVM", ex.0);
    }
    report(started, vm);
    code
}

/// As JNI `ExceptionDescribe`: a header, then the exception's own `printStackTrace()`.
fn describe_uncaught(t: &mut Thread, thread: &str, ex: Oop) {
    eprint!("Exception in thread \"{thread}\" ");
    // SAFETY: a live Throwable.
    let k = unsafe { crate::heap::obj::klass(ex) };
    let print = crate::link::lookup_method(k, Symbol::intern("printStackTrace"), Symbol::intern("()V")).expect("Throwable.printStackTrace");
    if t.call(print, &[ex as u64]).is_err() {
        eprint!("{}", crate::natives::describe_throwable(ex));
    }
}

fn execute(t: &mut Thread, line: &CommandLine) -> JResult<ExitCode> {
    if let Some(flag) = &line.show_settings {
        let flag = rt::new_string(t, flag)?;
        call_static(t, HELPER, "showSettings", "(ZLjava/lang/String;JJJ)V", &[1, flag as u64, 0, 0, 0, 0, 0, 0])?;
    }
    if let Some(version) = &line.version {
        call_static(t, "java/lang/VersionProps", "print", "(Z)V", &[version.to_stderr as u64])?;
        if version.then_exit {
            return Ok(ExitCode::SUCCESS);
        }
    }
    if let Some(query) = &line.query {
        answer(t, query)?;
        return Ok(ExitCode::SUCCESS);
    }
    let Some(launch) = &line.launch else {
        if line.show_settings.is_some() {
            return Ok(ExitCode::SUCCESS);
        }
        print_help(t, true)?;
        return Ok(ExitCode::FAILURE);
    };
    let name = rt::new_string(t, launch.target.name())?;
    let mirror = call_static(t, HELPER, "checkAndLoadMain", "(ZILjava/lang/String;)Ljava/lang/Class;", &[1, launch.target.mode() as u64, name as u64])?;
    let main = rt::klass_of_mirror(mirror as Oop).expect("main class mirror");
    if !line.dry_run {
        run_main(t, main, &launch.args)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn answer(t: &mut Thread, query: &Query) -> JResult<()> {
    match query {
        Query::Help { extra: false, to_stderr } => print_help(t, *to_stderr)?,
        Query::Help { extra: true, to_stderr } => {
            call_static(t, HELPER, "printXUsageMessage", "(Z)V", &[*to_stderr as u64])?;
        }
        Query::ListModules => {
            call_static(t, HELPER, "listModules", "()V", &[])?;
        }
        Query::DescribeModule(module) => {
            let module = rt::new_string(t, module)?;
            call_static(t, HELPER, "describeModule", "(Ljava/lang/String;)V", &[module as u64])?;
        }
        Query::ValidateModules => {}
    }
    Ok(())
}

fn print_help(t: &mut Thread, to_stderr: bool) -> JResult<()> {
    let program = rt::new_string(t, "rava")?;
    call_static(t, HELPER, "initHelpMessage", "(Ljava/lang/String;)V", &[program as u64])?;
    call_static(t, HELPER, "printHelpMessage", "(Z)V", &[to_stderr as u64])?;
    Ok(())
}

/// The kind of `main` `checkAndLoadMain` validated (`MAIN_NONSTATIC`, `MAIN_WITHOUT_ARGS`):
/// `getMainType()` through JDK 24; JDK 25's launcher reads the `isStaticMain` and
/// `noArgMain` statics instead.
fn main_type(t: &mut Thread) -> JResult<i32> {
    let helper = crate::loader::load(HELPER).expect("LauncherHelper");
    if helper.declared_method(Symbol::intern("getMainType"), Symbol::intern("()I")).is_some() {
        return Ok(call_static(t, HELPER, "getMainType", "()I", &[])? as i32);
    }
    let flag = |name: &str| {
        helper
            .fields
            .iter()
            .find(|f| f.is_static() && f.name.as_str() == name)
            .is_some_and(|f| helper.statics[f.offset as usize].load(std::sync::atomic::Ordering::Acquire) != 0)
    };
    let mut kind = 0;
    if !flag("isStaticMain") {
        kind |= MAIN_NONSTATIC;
    }
    if flag("noArgMain") {
        kind |= MAIN_WITHOUT_ARGS;
    }
    Ok(kind)
}

/// Invokes the `main` that `checkAndLoadMain` chose: static or instance, with or without `args`.
fn run_main(t: &mut Thread, k: &'static Klass, args: &[String]) -> JResult<()> {
    rt::initialize(t, k)?;
    let kind = main_type(t)?;
    let desc = if kind & MAIN_WITHOUT_ARGS != 0 { "()V" } else { "([Ljava/lang/String;)V" };
    let main = crate::link::lookup_method(k, Symbol::intern("main"), Symbol::intern(desc)).expect("main validated by LauncherHelper");
    let mut values = Vec::new();
    if kind & MAIN_NONSTATIC != 0 {
        let init = k.declared_method(Symbol::intern("<init>"), Symbol::intern("()V")).expect("constructor validated by LauncherHelper");
        let receiver = rt::alloc_instance(t, k)?;
        t.call(init, &[receiver as u64])?;
        values.push(receiver as u64);
    }
    if kind & MAIN_WITHOUT_ARGS == 0 {
        let args: Vec<Option<String>> = args.iter().cloned().map(Some).collect();
        values.push(crate::natives::string_array(t, &args)? as u64);
    }
    t.call(main, &values)?;
    Ok(())
}

fn timing() -> bool {
    std::env::var_os("RAVA_TIMING").is_some()
}

fn report(started: Instant, vm: &Vm) {
    if let Some(stats) = crate::jit::stats::get() {
        stats.report();
    }
    if timing() {
        let millis = started.elapsed().as_secs_f64() * 1e3;
        eprintln!("[rava] {millis:.1} ms, {} classes, {} KiB heap", vm.loaded_class_count(), vm.heap.used_bytes() / 1024);
    }
    if std::env::var_os("RAVA_PROFILE").is_some() {
        use std::sync::atomic::Ordering::Relaxed;
        let ms = |ns: u64| ns as f64 / 1e6;
        eprintln!(
            "[profile] parse {:.0} ms, compile {:.0} ms (summed over threads), wall {:.0} ms",
            ms(crate::classfile::PARSE_NS.load(Relaxed)),
            ms(crate::jit::queue::COMPILE_NS.load(Relaxed)),
            started.elapsed().as_secs_f64() * 1e3
        );
    }
}

/// A size as the command line writes one (`512m`, `4G`).
pub(crate) fn parse_vm_size(s: &str) -> Option<usize> {
    settings::parse_size(s)
}
