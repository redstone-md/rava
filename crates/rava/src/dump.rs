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

/// Prints a dump of every Java thread to standard output. When a collection (or a compiler
/// reading the heap) holds the world and does not let go within two seconds, the dump says who
/// holds it and still lists every thread, with the stacks of those that hold still.
pub fn print() {
    let patience = std::time::Duration::from_secs(2);
    let text = safepoint::try_with_world_stopped(patience, || collect(None)).unwrap_or_else(|| collect(Some(safepoint::holder())));
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// A thread's Java name, or what it is while it has none.
pub(crate) fn thread_name(rec: &safepoint::Record) -> String {
    let t = rec.thread.load(SeqCst);
    if t.is_null() {
        return "<detached>".to_string();
    }
    // SAFETY: a registered thread's `Thread` lives until it detaches.
    let jt = unsafe { (*t).java_thread };
    if jt.is_null() {
        return "<attaching>".to_string();
    }
    // SAFETY: a live java.lang.Thread; its name is a String or null.
    let n = unsafe { crate::heap::obj::read_ref(jt, name_offset()) };
    if n.is_null() { "<unnamed>".to_string() } else { rt::java_string_to_rust(n) }
}

/// The dump's text. With the world stopped by the dump itself (`held` None), every stopped or
/// native thread's frames hold still; when another holder has it (`held`), only while a stop is
/// requested.
fn collect(held: Option<String>) -> String {
    let mut s = match &held {
        None => String::from("\nFull thread dump rava:\n"),
        Some(h) => format!("\nFull thread dump rava (the world could not be stopped for the dump: {h}):\n"),
    };
    let frames_still = held.is_none() || safepoint::requested();
    for rec in safepoint::all_threads() {
        let t = rec.thread.load(SeqCst);
        if t.is_null() {
            continue;
        }
        // SAFETY: as in `thread_name`.
        let t = unsafe { &*t };
        let state = rec.state.load(SeqCst);
        let text = match state {
            NATIVE => "outside Java (blocked or in native code)",
            STOPPED => "in Java (at a safepoint)",
            _ => "running Java, not at a safepoint (frames not shown)",
        };
        let _ = writeln!(s, "\n\"{}\" {text}", thread_name(rec));
        if state == RUNNING || !frames_still {
            continue;
        }
        let monitor = rec.waiting_on.load(SeqCst);
        if monitor != 0 {
            let _ = writeln!(s, "   waiting on the monitor of {monitor:#x}");
        }
        for (m, bci) in t.frames().take(200) {
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
