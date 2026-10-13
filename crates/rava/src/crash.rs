//! Crash reports, as HotSpot's `hs_err_pid` file and bun's report link. A fault that nothing
//! resolves, or a panic that nothing catches, writes what the process knows (where it stopped,
//! the native frames, the Java frames of the thread) to stderr and to `rava_crash_<pid>.log` in
//! the working directory, then prints a link that opens a GitHub issue with those fields filled
//! in; the reporter reads the form before sending it. The link carries no command line (a game's
//! holds its account token) and no paths, only module names and Java frames.
//!
//! Everything formats into static buffers, one report per process: a fault handler must not
//! allocate (the fault may be inside the allocator), and on Unix it runs on a small signal stack.

use std::cell::{Cell, UnsafeCell};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

const REPO: &str = "redstone-md/rava";
/// The issue form the link fills in (`.github/ISSUE_TEMPLATE/crash.yml`).
const TEMPLATE: &str = "crash.yml";
/// How much of the frames the link carries (percent-encoding about triples it, and a link must
/// stay well below the 8 KiB a browser takes).
const LINK_JAVA: usize = 1200;
const LINK_NATIVE: usize = 500;

/// Text formatted into a fixed buffer; what does not fit is dropped.
struct Buf<const N: usize> {
    buf: [u8; N],
    n: usize,
}

impl<const N: usize> Buf<N> {
    const fn new() -> Self {
        Buf { buf: [0; N], n: 0 }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.n]).unwrap_or("")
    }

    fn clear(&mut self) {
        self.n = 0;
    }
}

impl<const N: usize> std::fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let mut take = s.len().min(N - self.n);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.n..self.n + take].copy_from_slice(&s.as_bytes()[..take]);
        self.n += take;
        Ok(())
    }
}

/// The report's buffers, used by the one thread that claims the report.
struct Scratch {
    /// The report's text so far, written out section by section.
    out: Buf<4096>,
    java: Buf<4096>,
    /// Native frames as `module+offset` lines, for the link.
    native: Buf<1024>,
    link: Buf<8192>,
}

struct Shared(UnsafeCell<Scratch>);

// SAFETY: only the thread that wins `CLAIMED` ever touches the cell.
unsafe impl Sync for Shared {}

static SCRATCH: Shared = Shared(UnsafeCell::new(Scratch { out: Buf::new(), java: Buf::new(), native: Buf::new(), link: Buf::new() }));
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// What crashed.
#[derive(Clone, Copy)]
enum Kind<'a> {
    Fault { pc: usize, addr: usize },
    Panic { message: &'a str, location: &'a str },
}

/// Reports a fault at `pc` touching `addr` that no handler resolves.
pub fn fault(pc: usize, addr: usize) {
    report(Kind::Fault { pc, addr });
}

thread_local!(static EXPECTED: Cell<u32> = const { Cell::new(0) });

/// Runs `f`, inside which a panic is caught and handled (a compilation that bails out), so it
/// is not a crash to report.
pub fn expected<R>(f: impl FnOnce() -> R) -> R {
    EXPECTED.with(|d| d.set(d.get() + 1));
    let r = f();
    EXPECTED.with(|d| d.set(d.get() - 1));
    r
}

/// Reports panics that nothing expects, then lets the default hook print its message.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if EXPECTED.with(|d| d.get()) == 0 {
            let message = info.payload().downcast_ref::<&str>().copied().or_else(|| info.payload().downcast_ref::<String>().map(String::as_str)).unwrap_or("(no message)");
            let mut location = Buf::<256>::new();
            if let Some(l) = info.location() {
                let _ = write!(location, "{}:{}", l.file(), l.line());
            }
            report(Kind::Panic { message, location: location.as_str() });
        }
        previous(info);
    }));
}

/// Writes `pc` as the module holding it and the offset there (for a debugger, or addr2line).
fn place(w: &mut impl std::fmt::Write, pc: usize, with_address: bool) {
    let mut name = [0u8; 128];
    if with_address {
        let _ = write!(w, "{pc:#x} ");
    }
    let _ = match crate::os::module(pc, &mut name) {
        Some((base, len)) => write!(w, "{}+{:#x}", std::str::from_utf8(&name[..len]).unwrap_or("?"), pc - base),
        None if with_address => Ok(()),
        None => write!(w, "{pc:#x}"),
    };
}

/// The system as the issue form's dropdown names it.
fn system() -> &'static str {
    if cfg!(windows) {
        "Windows x86-64"
    } else if cfg!(target_os = "macos") {
        "macOS (Intel)"
    } else {
        "Linux x86-64"
    }
}

fn report(kind: Kind) {
    if CLAIMED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: this thread won the claim; no other ever reaches the buffers.
    let s = unsafe { &mut *SCRATCH.0.get() };
    let mut name = Buf::<64>::new();
    let _ = write!(name, "rava_crash_{}.log\0", std::process::id());
    let file = crate::os::create_report(&name.buf[..name.n]);
    let file_name = &name.as_str()[..name.n - 1];
    let emit = |text: &str| {
        crate::os::write_stderr(text.as_bytes());
        if let Some(f) = file {
            crate::os::write_report(f, text.as_bytes());
        }
    };

    // Where it stopped, first: the rest walks the stack and may fault again.
    let mut summary = Buf::<512>::new();
    match kind {
        Kind::Fault { pc, addr } => {
            let _ = write!(summary, "memory fault in ");
            place(&mut summary, pc, false);
            let _ = write!(summary, " (address {addr:#x})");
        }
        Kind::Panic { message, location } => {
            let _ = write!(summary, "panic at {location}: {message}");
        }
    }
    s.out.clear();
    let _ = writeln!(s.out, "rava: crashed: {}", summary.as_str());
    let _ = writeln!(s.out, "rava {}, {}", env!("CARGO_PKG_VERSION"), system());
    emit(s.out.as_str());

    // The native frames, from the faulting instruction on (the ones before it are this handler
    // and the system's exception dispatch).
    let mut frames = [0usize; 48];
    let n = crate::os::native_frames(&mut frames);
    let frames = &frames[..n];
    let frames = frames.iter().position(|&f| matches!(kind, Kind::Fault { pc, .. } if f == pc)).map_or(frames, |i| &frames[i..]);
    s.out.clear();
    s.native.clear();
    let _ = writeln!(s.out, "native frames:");
    for &f in frames.iter().take_while(|&&f| f != 0) {
        let _ = write!(s.out, "    ");
        place(&mut s.out, f, true);
        let _ = writeln!(s.out);
        place(&mut s.native, f, false);
        let _ = writeln!(s.native);
    }
    emit(s.out.as_str());

    s.java.clear();
    crate::dump::write_current_thread(&mut s.java);
    emit(s.java.as_str());

    s.link.clear();
    link(s, summary.as_str());
    s.out.clear();
    if file.is_some() {
        let _ = writeln!(s.out, "The report was written to {file_name}.");
    }
    let _ = writeln!(s.out, "Please report this crash; the link opens a filled-in issue, and nothing is sent until you submit it:");
    let _ = writeln!(s.out, "{}", s.link.as_str());
    emit(s.out.as_str());
    if let Some(f) = file {
        crate::os::close_report(f);
    }
}

/// The issue link: the form's fields as query parameters (GitHub fills in a field whose id is
/// a parameter).
fn link(s: &mut Scratch, summary: &str) {
    let _ = write!(s.link, "https://github.com/{REPO}/issues/new?template={TEMPLATE}");
    let mut title = Buf::<160>::new();
    let _ = write!(title, "Crash: {summary}");
    param(&mut s.link, "title", title.as_str());
    param(&mut s.link, "version", env!("CARGO_PKG_VERSION"));
    param(&mut s.link, "system", system());
    param(&mut s.link, "where", summary);
    param(&mut s.link, "java", prefix(s.java.as_str(), LINK_JAVA));
    param(&mut s.link, "native", prefix(s.native.as_str(), LINK_NATIVE));
}

/// `s` cut to at most `n` bytes, at a line end where there is one.
fn prefix(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut cut = n;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].rfind('\n').map_or(&s[..cut], |i| &s[..i])
}

/// Appends `&key=value`, the value percent-encoded.
fn param(w: &mut Buf<8192>, key: &str, value: &str) {
    let _ = write!(w, "&{key}=");
    for b in value.bytes() {
        let _ = if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { w.write_char(b as char) } else { write!(w, "%{b:02X}") };
    }
}
