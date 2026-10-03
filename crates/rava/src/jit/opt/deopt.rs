//! Deoptimization: compiled code has written the interpreter state of every scope at the failing
//! point into the frame (the method's own and the inlined callees' frames at the arguments of
//! their calls). The interpreter finishes the innermost scope, then each caller from after its
//! call (or from the call, rethrowing), up to the method itself.

use super::regalloc::{Loc, XMMS};
use crate::jit::analysis::insn_len;
use crate::jit::asm::R;
use crate::klass::Klass;
use crate::method::Method;
use crate::thread::{header, Thread, Throw};
use crate::types::BasicType;
use crate::vm::vm;

/// Registers the common deoptimization stub pushes, in push order (an even count keeps the
/// stack aligned), above the SSE registers it stores (`XMMS` order, 8 bytes each); the runtime
/// finds the spill slots right above them.
pub const SAVED: [R; 12] = [R::Rbx, R::Rbp, R::R14, R::R15, R::Rsi, R::Rdi, R::R8, R::R9, R::R10, R::R11, R::Rcx, R::Rdx];

/// A deoptimization point: its scopes and where each live frame slot's value is.
pub struct Desc {
    /// Innermost first.
    pub frames: Vec<Frame>,
    /// (frame slot, location, whether an int to sign-extend) of every value to write back.
    pub slots: Vec<(i32, Loc, bool)>,
    /// Objects whose allocation was removed (`escape.rs`), rebuilt here into their frame slots.
    pub remats: Vec<Remat>,
}

/// A replaced allocation to rebuild on deoptimization: a fresh instance of `klass` with each
/// field written from where its value lives, its pointer stored into every frame slot that held
/// the object.
pub struct Remat {
    pub klass: &'static Klass,
    pub fields: Vec<(u32, Loc, BasicType)>,
    pub slots: Vec<i32>,
}

/// Descriptors are kept encoded (a few bytes per value, LEB128) and decoded only when a point
/// deoptimizes, which is rare: as structs of vectors they were the largest metadata of
/// optimized code.
impl Desc {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer(Vec::with_capacity(16 + 3 * self.slots.len()));
        w.u(self.frames.len() as u64);
        for f in &self.frames {
            w.u(f.method as *const Method as u64);
            w.i(f.frame as i64);
            w.u(f.bci as u64);
            w.u(f.depth as u64);
        }
        w.u(self.slots.len() as u64);
        for &(slot, loc, int) in &self.slots {
            w.i(slot as i64);
            w.loc(loc, int);
        }
        w.u(self.remats.len() as u64);
        for r in &self.remats {
            w.u(r.klass as *const Klass as u64);
            w.u(r.fields.len() as u64);
            for &(offset, loc, ty) in &r.fields {
                w.u(offset as u64);
                w.loc(loc, false);
                w.u(ty as u64);
            }
            w.u(r.slots.len() as u64);
            for &slot in &r.slots {
                w.i(slot as i64);
            }
        }
        w.0
    }

    /// # Safety
    /// `at` must be an encoding made by `encode`.
    pub unsafe fn decode(at: *const u8) -> Desc {
        let mut r = Reader(at);
        let frames = (0..r.u())
            .map(|_| Frame { method: &*(r.u() as *const Method), frame: r.i() as i32, bci: r.u() as u32, depth: r.u() as u32 })
            .collect();
        let slots = (0..r.u())
            .map(|_| {
                let slot = r.i() as i32;
                let (loc, int) = r.loc();
                (slot, loc, int)
            })
            .collect();
        let remats = (0..r.u())
            .map(|_| {
                let klass = &*(r.u() as *const Klass);
                let fields = (0..r.u()).map(|_| (r.u() as u32, r.loc().0, std::mem::transmute::<u8, BasicType>(r.u() as u8))).collect();
                let slots = (0..r.u()).map(|_| r.i() as i32).collect();
                Remat { klass, fields, slots }
            })
            .collect();
        Desc { frames, slots, remats }
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }

    fn i(&mut self, v: i64) {
        self.u(((v << 1) ^ (v >> 63)) as u64);
    }

    /// A location, its kind in the low three bits of a tag that also carries `int`.
    fn loc(&mut self, loc: Loc, int: bool) {
        let int = (int as u8) << 3;
        match loc {
            Loc::None => self.0.push(int),
            Loc::Reg(r) => self.0.extend([1 | int, r as u8]),
            Loc::Xmm(x) => self.0.extend([2 | int, x]),
            Loc::Stack(k) => {
                self.0.push(3 | int);
                self.i(k as i64);
            }
            Loc::Frame(k) => {
                self.0.push(4 | int);
                self.i(k as i64);
            }
            Loc::Const(c) => {
                self.0.push(5 | int);
                self.i(c);
            }
        }
    }
}

struct Reader(*const u8);

impl Reader {
    unsafe fn byte(&mut self) -> u8 {
        let b = *self.0;
        self.0 = self.0.add(1);
        b
    }

    unsafe fn u(&mut self) -> u64 {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = self.byte();
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }

    unsafe fn i(&mut self) -> i64 {
        let v = self.u();
        (v >> 1) as i64 ^ -((v & 1) as i64)
    }

    unsafe fn loc(&mut self) -> (Loc, bool) {
        let tag = self.byte();
        let loc = match tag & 7 {
            0 => Loc::None,
            // SAFETY: written from an `R` by `Writer::loc`.
            1 => Loc::Reg(std::mem::transmute::<u8, R>(self.byte())),
            2 => Loc::Xmm(self.byte()),
            3 => Loc::Stack(self.i() as i32),
            4 => Loc::Frame(self.i() as i32),
            _ => Loc::Const(self.i()),
        };
        (loc, tag & 8 != 0)
    }
}

/// One scope of a deoptimization point.
pub struct Frame {
    pub method: &'static Method,
    /// Frame slot of its local 0.
    pub frame: i32,
    pub bci: u32,
    /// Operand-stack depth at `bci`.
    pub depth: u32,
}

/// A deoptimization before a virtual or interface call (a receiver-type guard that failed):
/// records the receiver's class in the call site's profile, for the recompilation.
fn note_receiver(f: &Frame, base: *mut u64) {
    let code = f.method.code.expect("compiled scope has code");
    if !matches!(code.code[f.bci as usize], 0xb6 | 0xb9) {
        return;
    }
    let pc = f.bci as usize;
    let index = u16::from_be_bytes([code.code[pc + 1], code.code[pc + 2]]) as usize;
    let Some(crate::link::Resolved::Method(m)) = f.method.holder.resolved[index].get() else { return };
    // A signature-polymorphic call's arguments follow the site's descriptor, not `m`'s.
    if m.is_signature_polymorphic() {
        return;
    }
    let Some(at) = (f.depth as usize).checked_sub(m.param_slots() as usize) else { return };
    // SAFETY: the receiver's operand slot of this scope, just written back.
    let receiver = unsafe { *base.add(f.frame as usize + code.max_locals as usize + header::SLOTS + at) } as crate::heap::Oop;
    if !receiver.is_null() {
        // SAFETY: a live receiver.
        crate::jit::profile::note_receiver(f.method, pc, unsafe { crate::heap::obj::klass(receiver) });
    }
}

/// Writes the state of deoptimization point `desc` into the compiled frame at `base` (register
/// values from `saved`, as the common stub pushed them), then resumes its scopes innermost
/// first, with the exception a call left pending rethrown at the innermost one. Returns the
/// method's result; an exception it throws is left pending.
pub extern "sysv64" fn deopt(t: *mut Thread, base: *mut u64, desc: *const u8, saved: *const u64) -> u64 {
    // SAFETY: called by compiled code with its own thread, frame, encoded descriptor and saved
    // registers.
    let (t, desc) = unsafe { (&mut *t, &Desc::decode(desc)) };
    let gprs = XMMS.len();
    // Where a value currently lives. SAFETY: the stub stored the SSE registers, and above them
    // pushed `SAVED` last to first, below the spill slots.
    let read = |loc: Loc| unsafe {
        match loc {
            Loc::Xmm(x) => *saved.add(XMMS.iter().position(|&s| s == x).expect("an allocatable SSE register")),
            Loc::Reg(r) => *saved.add(gprs + SAVED.len() - 1 - SAVED.iter().position(|&s| s == r).expect("an allocatable register")),
            Loc::Stack(k) => *saved.add(gprs + SAVED.len() + k as usize),
            Loc::Frame(k) => *base.add(k as usize),
            Loc::Const(c) => c as u64,
            Loc::None => 0,
        }
    };
    // Every value is read before any slot is written: a parameter is read from its own frame
    // slot, which the state may give another value. Replaced objects' field values are read now
    // too, so the write-back cannot clobber them; references among them stay rooted on this
    // stack across the allocation below (the collector it may trigger does not move objects).
    let mut values = Vec::with_capacity(desc.slots.len());
    for &(slot, loc, int) in &desc.slots {
        if loc == Loc::None {
            continue;
        }
        let v = read(loc);
        values.push((slot, if int { v as i32 as i64 as u64 } else { v }));
    }
    let remat_fields: Vec<Vec<u64>> = desc.remats.iter().map(|r| r.fields.iter().map(|&(_, loc, _)| read(loc)).collect()).collect();
    for &(slot, v) in &values {
        // SAFETY: a slot of the compiled frame.
        unsafe { *base.add(slot as usize) = v };
    }
    for (r, fields) in desc.remats.iter().zip(&remat_fields) {
        let mem = vm().heap.allocate(t, r.klass.instance_size() as usize).expect("heap space to rebuild a scalar-replaced object on deopt");
        // SAFETY: fresh zeroed memory of the instance size.
        let obj = unsafe { crate::heap::init_instance(mem, r.klass) };
        for (&(offset, _, ty), &raw) in r.fields.iter().zip(fields) {
            // SAFETY: a field of the fresh instance, written at its declared width.
            unsafe {
                match ty {
                    BasicType::Boolean | BasicType::Byte => crate::heap::obj::write::<u8>(obj, offset, raw as u8),
                    BasicType::Char | BasicType::Short => crate::heap::obj::write::<u16>(obj, offset, raw as u16),
                    BasicType::Int | BasicType::Float => crate::heap::obj::write::<u32>(obj, offset, raw as u32),
                    _ => crate::heap::obj::write::<u64>(obj, offset, raw),
                }
            }
        }
        for &slot in &r.slots {
            // SAFETY: a slot of the compiled frame.
            unsafe { *base.add(slot as usize) = obj as u64 };
        }
    }
    let (frames, n) = (&desc.frames[..], desc.frames.len());
    if let Some(s) = crate::jit::stats::get() {
        s.deopts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    super::note_deopt(frames[n - 1].method);
    let mut outcome: Result<u64, Throw> = match t.take_pending() {
        Some(e) => Err(e),
        None => Ok(0),
    };
    if outcome.is_ok() {
        crate::jit::profile::note_trap(frames[0].method, frames[0].bci as usize);
        note_receiver(&frames[0], base);
    }
    if env_once!("RAVA_DEOPT_LOG").is_some() {
        let scopes: Vec<String> = frames.iter().map(|f| format!("{}@{} depth {} frame {}", f.method, f.bci, f.depth, f.frame)).collect();
        let pending = match &outcome {
            Err(e) => crate::runtime::class_name_of(e.0),
            Ok(_) => "-".into(),
        };
        let op = frames[0].method.code.map_or(0, |c| c.code[frames[0].bci as usize]);
        eprintln!("[deopt] {} op={op:#04x} pending={pending}", scopes.join(" <- "));
    }
    // The inlined scopes get their interpreter frames linked first, outermost first and each at
    // its call site, so every scope running (and stack traces taken in it) sees its callers.
    let hdr = |f: &Frame| {
        let code = f.method.code.expect("compiled scope has code");
        // SAFETY: the header of this scope's frame, within the compiled frame.
        unsafe { base.add(f.frame as usize + code.max_locals as usize) }
    };
    // Optimized code does not save `Thread::sp` in its header, but code that takes the frame
    // over (baseline code entered on a loop) restores it on return: `sp` as the stub published
    // it (at or above the caller's frame end, as callers need).
    // SAFETY: the outermost scope's header, within the compiled frame.
    unsafe { *hdr(&frames[n - 1]).add(header::SAVED_SP) = t.sp as u64 };
    // The inlined scopes become frames of their own: the physical header stops describing them
    // (an `InlinedSite` of the last call).
    // SAFETY: as above.
    unsafe { *hdr(&frames[n - 1]).add(header::BCI) = (frames[n - 1].bci as u64) << 1 };
    for f in frames[..n - 1].iter().rev() {
        let code = f.method.code.expect("compiled scope has code");
        // SAFETY: header and operand stack of this scope's frame, within the compiled frame.
        unsafe {
            t.push_frame(hdr(f), f.method, (f.bci as u64) << 1);
            t.sp = hdr(f).add(header::SLOTS + code.max_stack as usize + 2);
        }
    }
    for (i, f) in frames.iter().enumerate() {
        let code = f.method.code.expect("compiled scope has code");
        // SAFETY: the scope's frame lies within the compiled frame.
        let locals = unsafe { base.add(f.frame as usize) };
        let (bci, depth, pending) = match (i, outcome) {
            (0, Ok(_)) => (f.bci as usize, f.depth as usize, None),
            (_, Err(e)) => (f.bci as usize, f.depth as usize, Some(e)),
            // The callee returned: push its result and continue after the call.
            (_, Ok(v)) => {
                let slots = frames[i - 1].method.sig.ret.slots() as usize;
                // SAFETY: operand slot `depth` of this scope (the callee's first local).
                unsafe { *locals.add(code.max_locals as usize + header::SLOTS + f.depth as usize) = v };
                (f.bci as usize + insn_len(code.code, f.bci as usize), f.depth as usize + slots, None)
            }
        };
        // An inlined synchronized method holds its monitor (the outermost scope's is released by
        // the compiled code this returns to): the interpreter finishes the method, then it is
        // released, as `interp::interpret` releases it on any exit.
        let lock = (i + 1 < n && f.method.is_synchronized()).then(|| {
            // SAFETY: local 0 of an inlined synchronized instance method is kept in every state.
            if f.method.is_static() { f.method.holder.mirror() } else { unsafe { *locals as crate::heap::Oop } }
        });
        outcome = crate::interp::resume(t, f.method, locals, bci, depth, pending);
        if let Some(o) = lock {
            crate::sync::exit(o);
        }
        if i + 1 < n {
            // SAFETY: this scope's frame is the innermost linked one.
            unsafe { t.pop_frame(hdr(f)) };
        }
    }
    match outcome {
        Ok(v) => v,
        Err(e) => {
            t.pending = e.0;
            0
        }
    }
}
