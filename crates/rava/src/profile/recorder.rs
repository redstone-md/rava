//! The recorder's thread (Windows): samples the registered threads every interval, resolves
//! the captured words into frames, interns strings, methods, modules and stacks, and writes the
//! records (`format.rs`). Once a second it also writes each thread's name and CPU, the modules
//! loaded since, and flushes, so a killed run keeps all but its last second.

use super::capture::{self, Raw};
use super::format::{Frame, Record, State, Tier, Writer};
use super::Config;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufWriter;
use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Code installed since the recorder last looked: start, size, name.
static INSTALLED: Mutex<Vec<(usize, usize, String)>> = Mutex::new(Vec::new());

/// 1 once the VM asks the recorder to finish, 2 once it has.
static FINISH: AtomicU8 = AtomicU8::new(0);

pub fn finish() {
    FINISH.store(1, SeqCst);
    let t = Instant::now();
    while FINISH.load(SeqCst) != 2 && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Inlining maps of optimized code installed since the recorder last looked (`code_scopes`).
type Scopes = Vec<(u32, Vec<(&'static crate::method::Method, i32)>)>;
static SCOPED: Mutex<Vec<(usize, Scopes)>> = Mutex::new(Vec::new());

pub fn code_scopes(start: usize, scopes: Scopes) {
    SCOPED.lock().unwrap().push((start, scopes));
}

pub fn code_installed(start: usize, size: usize, name: &str) {
    INSTALLED.lock().unwrap().push((start, size, name.to_string()));
}

pub fn start(config: Config) -> bool {
    let file = match File::create(&config.path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("rava: RAVA_RECORD: cannot create {}: {e}", config.path.display());
            return false;
        }
    };
    let Ok(writer) = Writer::new(BufWriter::with_capacity(1 << 20, file)) else { return false };
    let mut r = Recorder { out: writer, strings: HashMap::new(), methods: HashMap::new(), stacks: HashMap::new(), code: BTreeMap::new(), scopes: HashMap::new(), chains: Vec::new(), chain_ids: HashMap::new(), modules: Vec::new(), threads: HashMap::new(), loaded: Vec::new() };
    r.meta("vm", &format!("rava {}", env!("CARGO_PKG_VERSION")));
    r.meta("pid", &std::process::id().to_string());
    r.meta("command", &std::env::args().collect::<Vec<_>>().join(" "));
    r.meta("start_unix_ms", &std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()).to_string());
    r.meta("cycles_per_second", &calibrate().to_string());
    r.meta("interval_us", &config.interval.to_string());
    r.meta("mode", if config.wall { "wall" } else { "cpu" });
    let spawned = std::thread::Builder::new().name("rava-recorder".into()).spawn(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.run(config))).is_err() {
            eprintln!("rava: RAVA_RECORD: the recorder stopped; the recording ends here");
        }
    });
    spawned.is_ok()
}

/// The rate of the cycle counter `QueryThreadCycleTime` reads (the time stamp counter).
fn calibrate() -> u64 {
    // SAFETY: reading the time stamp counter has no preconditions on x86-64.
    let tsc = || unsafe { std::arch::x86_64::_rdtsc() };
    let (t0, c0) = (Instant::now(), tsc());
    std::thread::sleep(Duration::from_millis(50));
    let (dt, dc) = (t0.elapsed().as_secs_f64(), tsc() - c0);
    (dc as f64 / dt) as u64
}

struct Module {
    id: u32,
    base: usize,
    end: usize,
    /// Its executable sections: only an address in one can be a return address.
    code: Vec<(usize, usize)>,
    /// ntdll's or win32u's: system call stubs.
    stubs: bool,
    /// A system library (whose callers name a wait or an I/O better than it does).
    system: bool,
    vm: bool,
}

struct Recorder {
    out: Writer<BufWriter<File>>,
    strings: HashMap<String, u32>,
    /// Method pointers to their name's string.
    methods: HashMap<usize, u32>,
    stacks: HashMap<Vec<Frame>, u32>,
    /// Compiled code by start: end, method string, tier.
    code: BTreeMap<usize, (usize, u32, Tier)>,
    /// Optimized code's inlining maps by start: from each offset on, a chain of frames.
    scopes: HashMap<usize, Vec<(u32, u32)>>,
    /// The chains (innermost first, the compiled method last), and their ids by their methods
    /// and bytecode indices.
    chains: Vec<Vec<Frame>>,
    chain_ids: HashMap<Vec<(usize, i32)>, u32>,
    modules: Vec<Module>,
    /// Thread ids to their name's string.
    threads: HashMap<u32, u32>,
    /// Each loaded class's methods (address, count), sorted: header words are checked against
    /// it before being read as methods.
    loaded: Vec<(usize, usize)>,
}

impl Recorder {
    fn meta(&mut self, key: &str, value: &str) {
        let _ = self.out.write(&Record::Meta { key: key.into(), value: value.into() });
    }

    fn string(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.strings.get(s) {
            return id;
        }
        let id = self.strings.len() as u32 + 1;
        self.strings.insert(s.to_string(), id);
        let _ = self.out.write(&Record::Str { id, text: s.to_string() });
        id
    }

    fn method(&mut self, m: usize) -> u32 {
        if let Some(&id) = self.methods.get(&m) {
            return id;
        }
        // SAFETY: callers checked `m` is a loaded class's method (`is_method`).
        let name = format!("{}", unsafe { &*(m as *const crate::method::Method) });
        let id = self.string(&name);
        self.methods.insert(m, id);
        id
    }

    fn run(mut self, config: Config) {
        let started = Instant::now();
        let mut raws = Vec::new();
        let mut last_second = Instant::now() - Duration::from_secs(2);
        let interval = Duration::from_micros(config.interval);
        loop {
            let tick = Instant::now();
            if FINISH.load(SeqCst) == 1 {
                self.each_second(started);
                FINISH.store(2, SeqCst);
                return;
            }
            if tick.duration_since(last_second) >= Duration::from_secs(1) {
                last_second = tick;
                self.each_second(started);
            }
            self.take_installed();
            let n = capture::sample(config.wall, &mut raws);
            let time = started.elapsed().as_micros() as u64;

            for raw in &raws[..n] {
                let (state, frames) = self.resolve(raw);

                let stack = self.stack(frames);
                let _ = self.out.write(&Record::Sample { time, thread: raw.thread, state, cycles: raw.cycles, stack });
            }
            if let Some(rest) = interval.checked_sub(tick.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    }

    fn each_second(&mut self, started: Instant) {
        let time = started.elapsed().as_micros() as u64;
        for (id, name, cycles) in capture::threads() {
            let n = self.string(&name);
            if self.threads.insert(id, n) != Some(n) {
                let _ = self.out.write(&Record::Thread { id, name: n });
            }
            let _ = self.out.write(&Record::ThreadCpu { time, thread: id, cycles });
        }
        self.refresh_modules();
        if let Some(vm) = crate::vm::created() {
            self.loaded = vm.all_classes.lock().unwrap().iter().map(|k| k.methods()).filter(|m| !m.is_empty()).map(|m| (m.as_ptr() as usize, m.len())).collect();
            self.loaded.sort_unstable();
        }
        let _ = self.out.flush();
    }

    fn take_installed(&mut self) {
        let installed = std::mem::take(&mut *INSTALLED.lock().unwrap());
        for (start, size, name) in installed {
            let (method, tier) = super::split_code_name(&name);
            let m = self.string(method);
            // Code replaced at the same place (a recompilation into freed space) overrides.
            self.code.retain(|&s, &mut (e, ..)| e <= start || s >= start + size);
            self.scopes.retain(|&s, _| s < start || s >= start + size);
            self.code.insert(start, (start + size, m, tier));
            let _ = self.out.write(&Record::Code { start: start as u64, size: size as u64, method: m, tier });
        }
        let scoped = std::mem::take(&mut *SCOPED.lock().unwrap());
        for (start, scopes) in scoped {
            let mut map = Vec::with_capacity(scopes.len());
            for (at, chain) in scopes {
                let key: Vec<(usize, i32)> = chain.iter().map(|&(m, b)| (m as *const _ as usize, b)).collect();
                let found = self.chain_ids.get(&key).copied();
                let id = match found {
                    Some(id) => id,
                    None => {
                        let n = chain.len();
                        let frames = chain.iter().enumerate().map(|(i, &(m, b))| Frame::Java { method: self.method(m as *const _ as usize), bci: (b >= 0).then_some(b as u32), tier: Tier::Opt, inlined: i + 1 < n }).collect();
                        self.chains.push(frames);
                        let id = self.chains.len() as u32 - 1;
                        self.chain_ids.insert(key, id);
                        id
                    }
                };
                map.push((at, id));
            }
            self.scopes.insert(start, map);
        }
    }

    fn refresh_modules(&mut self) {
        for (base, end, path) in modules() {
            if self.modules.iter().any(|m| m.base == base) {
                continue;
            }
            let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_ascii_lowercase();
            let system = matches!(file.as_str(), "ntdll.dll" | "win32u.dll" | "kernelbase.dll" | "kernel32.dll" | "user32.dll" | "gdi32.dll" | "gdi32full.dll" | "ws2_32.dll" | "mswsock.dll");
            let id = self.modules.len() as u32 + 1;
            let p = self.string(&path);
            let _ = self.out.write(&Record::Module { id, path: p, base: base as u64, size: (end - base) as u64 });
            let here = crate::profile::start as *const () as usize;
            let vm = here >= base && here < end;
            // SAFETY: the module is loaded at `base`; its headers are read in place.
            let code = unsafe { code_sections(base, end) };
            self.modules.push(Module { id, base, end, code, stubs: matches!(file.as_str(), "ntdll.dll" | "win32u.dll"), system, vm });
        }
    }

    fn module_of(&self, a: usize) -> Option<&Module> {
        self.modules.iter().find(|m| a >= m.base && a < m.end)
    }

    fn code_of(&self, a: usize) -> Option<(u32, Tier)> {
        self.region_of(a).map(|(_, m, tier)| (m, tier))
    }

    /// The compiled code holding `a`: its start, method and tier.
    fn region_of(&self, a: usize) -> Option<(usize, u32, Tier)> {
        match self.code.range(..=a).next_back() {
            Some((&start, &(end, m, tier))) if a < end => Some((start, m, tier)),
            _ => None,
        }
    }

    /// The frames at `pc` in optimized code at `start` with an inlining map (inlined methods
    /// innermost first, the compiled method last).
    fn inlined_at(&self, start: usize, pc: usize) -> Option<&[Frame]> {
        let map = self.scopes.get(&start)?;
        let i = map.partition_point(|&(at, _)| at as usize <= pc - start);
        (i > 0).then(|| self.chains[map[i - 1].1 as usize].as_slice())
    }

    fn native(&self, a: usize) -> Frame {
        match self.module_of(a) {
            Some(m) => Frame::Native { module: m.id, offset: (a - m.base) as u64 },
            None => Frame::Address(a as u64),
        }
    }

    /// Whether `m` is a method of a class loaded by the last second.
    fn is_method(&self, m: usize) -> bool {
        let size = std::mem::size_of::<crate::method::Method>();
        let i = self.loaded.partition_point(|&(start, _)| start <= m);
        i > 0 && {
            let (start, n) = self.loaded[i - 1];
            m < start + n * size && (m - start) % size == 0
        }
    }

    /// The sample's state and frames, innermost first.
    fn resolve(&mut self, raw: &Raw) -> (State, Vec<Frame>) {
        let mut frames = Vec::with_capacity(16);
        let mut leaf = None;
        let state = if let Some((start, m, tier)) = self.region_of(raw.pc) {
            match self.inlined_at(start, raw.pc) {
                Some(chain) => frames.extend_from_slice(chain),
                None => frames.push(Frame::Java { method: m, bci: None, tier, inlined: false }),
            }
            leaf = Some(m);
            State::Java
        } else {
            frames.push(self.native(raw.pc));
            let (stubs, system, vm) = self.module_of(raw.pc).map_or((false, false, false), |m| (m.stubs, m.system, m.vm));
            if stubs && raw.top != 0 {
                frames.push(self.native(raw.top));
            }
            // Who called into the system library or the VM: the first scanned return address
            // into compiled code or, past system libraries, into another module.
            let origin = raw.scanned.iter().copied().find(|&a| self.code_of(a).is_some() || self.module_of(a).is_some_and(|m| !m.system && (system || stubs) && m.code.iter().any(|&(b, e)| a >= b && a < e)));
            match origin.map(|a| (a, self.code_of(a))) {
                Some((_, Some((m, tier)))) => {
                    frames.push(Frame::Java { method: m, bci: None, tier, inlined: false });
                    leaf = Some(m);
                }
                Some((a, None)) => frames.push(self.native(a)),
                None => {}
            }
            if stubs {
                State::Syscall
            } else if vm {
                State::Vm
            } else {
                State::Native
            }
        };
        self.java_frames(raw, leaf, &mut frames);
        (state, frames)
    }

    /// Appends the frames of the headers: a header up to a few past `top` holding the leaf's
    /// method is the leaf's own frame (`top` stays at the last header published, and callees
    /// that returned since are dead), so the walk starts after it; a call site's inlining record
    /// stands for the frames inlined at it.
    fn java_frames(&mut self, raw: &Raw, leaf: Option<u32>, frames: &mut Vec<Frame>) {
        use crate::thread::header;
        let mut from = 0;
        if let Some(l) = leaf {
            for (i, &(m, ..)) in raw.headers.iter().take(8).enumerate() {
                if self.is_method(m) && self.method(m) == l {
                    from = i + 1;
                    break;
                }
            }
        }
        for &(m, bci, interpreted) in &raw.headers[from..] {
            if m == 0 || !self.is_method(m) {
                continue;
            }
            if !interpreted && (header::SITE_MIN..header::SITE_END).contains(&bci) && bci % 8 == 0 {
                let mut fat = [0usize; 2];
                if capture::copy(bci as usize, &mut fat) && fat[1] > 0 && fat[1] < 256 {
                    let mut pairs = vec![0usize; 2 * fat[1]];
                    if capture::copy(fat[0], &mut pairs) && pairs.chunks_exact(2).all(|f| self.is_method(f[0])) {
                        let n = fat[1];
                        for (i, f) in pairs.chunks_exact(2).enumerate() {
                            let method = self.method(f[0]);
                            let b = f[1] as i32;
                            frames.push(Frame::Java { method, bci: (b >= 0).then_some(b as u32), tier: Tier::Opt, inlined: i + 1 < n });
                        }
                        continue;
                    }
                }
            }
            let method = self.method(m);
            let b = (bci as i64) >> 1;
            let bci = (bci != u64::MAX && (0..=u16::MAX as i64).contains(&b)).then_some(b as u32);
            frames.push(Frame::Java { method, bci, tier: if interpreted { Tier::Interp } else { Tier::Unknown }, inlined: false });
        }
    }

    fn stack(&mut self, frames: Vec<Frame>) -> u32 {
        if let Some(&id) = self.stacks.get(&frames) {
            return id;
        }
        let id = self.stacks.len() as u32 + 1;
        let _ = self.out.write(&Record::Stack { id, frames: frames.clone() });
        self.stacks.insert(frames, id);
        id
    }
}

/// Every loaded module: base, end, path.
fn modules() -> Vec<(usize, usize, String)> {
    use windows_sys::Win32::System::ProcessStatus::{EnumProcessModules, GetModuleFileNameExW, GetModuleInformation, MODULEINFO};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut mods = vec![std::ptr::null_mut(); 1024];
    let mut needed = 0u32;
    // SAFETY: queries of this process into buffers of the sizes passed.
    unsafe {
        let p = GetCurrentProcess();
        if EnumProcessModules(p, mods.as_mut_ptr(), (mods.len() * 8) as u32, &mut needed) == 0 {
            return Vec::new();
        }
        mods.truncate((needed as usize / 8).min(mods.len()));
        mods.into_iter()
            .filter_map(|m| {
                let mut info: MODULEINFO = std::mem::zeroed();
                if GetModuleInformation(p, m, &mut info, std::mem::size_of::<MODULEINFO>() as u32) == 0 {
                    return None;
                }
                let mut name = [0u16; 520];
                let n = GetModuleFileNameExW(p, m, name.as_mut_ptr(), name.len() as u32) as usize;
                let base = info.lpBaseOfDll as usize;
                Some((base, base + info.SizeOfImage as usize, String::from_utf16_lossy(&name[..n])))
            })
            .collect()
    }
}

/// The executable sections of the PE image loaded at `base` (its section table), clamped to
/// the image.
///
/// # Safety
/// `base..end` is a loaded module's image.
unsafe fn code_sections(base: usize, end: usize) -> Vec<(usize, usize)> {
    const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
    let rd16 = |a: usize| (a as *const u16).read_unaligned() as usize;
    let rd32 = |a: usize| (a as *const u32).read_unaligned();
    if end - base < 0x400 || rd16(base) != 0x5a4d {
        return Vec::new();
    }
    let nt = base + rd32(base + 0x3c) as usize;
    if nt + 24 > end || rd32(nt) != 0x4550 {
        return Vec::new();
    }
    let sections = rd16(nt + 6);
    let table = nt + 24 + rd16(nt + 20);
    (0..sections)
        .map(|i| table + 40 * i)
        .take_while(|&s| s + 40 <= end)
        .filter(|&s| rd32(s + 36) & IMAGE_SCN_MEM_EXECUTE != 0)
        .map(|s| {
            let start = base + rd32(s + 12) as usize;
            (start, (start + rd32(s + 8) as usize).min(end))
        })
        .collect()
}