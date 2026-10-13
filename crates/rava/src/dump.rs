//! Thread dumps, as HotSpot prints them on `SIGQUIT` or Ctrl+Break: every Java thread with its
//! state and frames, taken with the world stopped and printed to standard output once it runs
//! again. `RAVA_DUMP_EVERY=<seconds>` also prints one at that interval, for programs started
//! without a console (a launcher that only keeps the output).

use crate::gc::safepoint::{self, NATIVE, RUNNING, STOPPED};
use crate::runtime::{self as rt, vm_field};
use std::fmt::Write;
use std::sync::atomic::Ordering::SeqCst;

vm_field!(name_offset, "java/lang/Thread", "name", "Ljava/lang/String;");

/// Installs the dump triggers (once the VM has booted).
pub fn install() {
    crate::os::on_dump_request(print);
    let every = std::env::var("RAVA_DUMP_EVERY").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&s| s > 0);
    if let Some(secs) = every {
        let _ = std::thread::Builder::new().name("rava-dump".into()).spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            print();
        });
    }
}

/// Prints a dump of every Java thread to standard output.
pub fn print() {
    let text = safepoint::with_world_stopped(std::time::Duration::from_secs(2), collect);
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// The dump's text; the world is stopped, so every thread's frames hold still.
fn collect() -> String {
    let mut s = String::from("\nFull thread dump rava:\n");
    for rec in safepoint::all_threads() {
        let t = rec.thread.load(SeqCst);
        if t.is_null() {
            continue;
        }
        // SAFETY: a registered thread's `Thread` lives until it detaches, which the registry
        // excludes while the world is stopped.
        let t = unsafe { &*t };
        let name = if t.java_thread.is_null() {
            "<attaching>".to_string()
        } else {
            // SAFETY: a live java.lang.Thread; its name is a String or null.
            let n = unsafe { crate::heap::obj::read_ref(t.java_thread, name_offset()) };
            if n.is_null() { "<unnamed>".to_string() } else { rt::java_string_to_rust(n) }
        };
        let state = rec.state.load(SeqCst);
        let text = match state {
            NATIVE => "outside Java (blocked or in native code)",
            STOPPED => "in Java (at a safepoint)",
            _ => "running Java, did not reach a safepoint (frames not shown)",
        };
        let _ = writeln!(s, "\n\"{name}\" {text}");
        if state == RUNNING {
            continue;
        }
        let monitor = rec.waiting_on.load(SeqCst);
        if monitor != 0 {
            let _ = writeln!(s, "   waiting on the monitor of {monitor:#x}");
        }
        for (m, bci) in t.frames() {
            let class = m.holder.name.as_str().replace('/', ".");
            let place = if m.is_native() {
                "Native Method".to_string()
            } else {
                let (file, line) = (crate::natives::source_file(m), crate::natives::line_number(m, bci));
                match (file, line >= 0) {
                    (Some(f), true) => format!("{f}:{line}"),
                    (Some(f), false) => f.to_string(),
                    (None, _) => "Unknown Source".to_string(),
                }
            };
            let _ = writeln!(s, "\tat {class}.{}({place})", m.name.as_str());
        }
    }
    s.push('\n');
    s
}
