//! `java.lang.StackWalker` support (`StackStreamFactory`), following HotSpot's protocol: the VM
//! captures the caller's frames, fills the Java-side buffer batch by batch (class mirrors, or
//! `StackFrameInfo` objects whose `MemberName` and bci it initializes) and calls back
//! `AbstractStackWalker.doStackWalk` with an anchor naming the walk.

use super::{ret_bool, ret_int, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::method::Method;
use crate::runtime::{self as rt, vm_field};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};

const FILL_CLASS_REFS_ONLY: u64 = 0x2;
const SHOW_HIDDEN_FRAMES: u64 = 0x20;
const W: &str = "java/lang/StackStreamFactory$AbstractStackWalker";

vm_field!(sfi_member_name, "java/lang/StackFrameInfo", "memberName", "Ljava/lang/Object;");

/// JDK 22+ frame objects (`ClassFrameInfo`, extended by `StackFrameInfo`): the declaring
/// class or the method's `ResolvedMethodName`, and member flags beside the walker's
/// retain-class-reference bit (`crate::jdk::Library::class_frame_info`).
fn cfi_field(cell: &'static std::sync::OnceLock<u32>, name: &str, desc: &str) -> Option<u32> {
    let lib = crate::jdk::library();
    lib.class_frame_info().then(|| lib.optional_field(cell, "java/lang/ClassFrameInfo", name, desc)).flatten()
}
fn cfi_target() -> Option<u32> {
    static CELL: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    cfi_field(&CELL, "classOrMemberName", "Ljava/lang/Object;")
}
fn cfi_flags() -> Option<u32> {
    static CELL: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    cfi_field(&CELL, "flags", "I")
}
vm_field!(sfi_name, "java/lang/StackFrameInfo", "name", "Ljava/lang/String;");
vm_field!(sfi_type, "java/lang/StackFrameInfo", "type", "Ljava/lang/Object;");
const RETAIN_CLASS_REF_BIT: i32 = 0x0800_0000;
vm_field!(sfi_bci, "java/lang/StackFrameInfo", "bci", "I");

/// The frames of one walk still to be handed out.
struct Walk {
    frames: Vec<(&'static Method, i32)>,
    next: usize,
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("java/lang/StackStreamFactory", "checkStackWalkModes", "()Z", |_, _| ret_bool(true));
    r.add(
        W,
        "callStackWalk",
        "(JILjdk/internal/vm/ContinuationScope;Ljdk/internal/vm/Continuation;II[Ljava/lang/Object;)Ljava/lang/Object;",
        // JDK 21: (long mode, int skip, scope, continuation, int batch, int start, frames).
        |t, a| call_stack_walk(t, a[0] as Oop, a[1], a[3] as i32 as usize, a[6] as i32, a[7] as i32, a[8] as Oop),
    );
    // JDK 25: (int mode, int skip, scope, continuation, int batch, int start, frames).
    r.add(
        W,
        "callStackWalk",
        "(IILjdk/internal/vm/ContinuationScope;Ljdk/internal/vm/Continuation;II[Ljava/lang/Object;)Ljava/lang/Object;",
        |t, a| call_stack_walk(t, a[0] as Oop, a[1] as u32 as u64, a[2] as i32 as usize, a[5] as i32, a[6] as i32, a[7] as Oop),
    );
    // JDK 25: the name and descriptor of the frame's method, on demand.
    r.add("java/lang/StackFrameInfo", "expandStackFrameInfo", "()V", |t, a| {
        let info = a[0] as Oop;
        let target = cfi_target().map(|off| unsafe { obj::read_ref(info, off) });
        let Some(m) = target.filter(|r| !r.is_null()).and_then(super::mh::method_of_resolved_name) else { return super::ret_void() };
        let name = rt::new_string(t, m.name.as_str())?;
        let desc = rt::new_string(t, m.desc.as_str())?;
        // SAFETY: a live StackFrameInfo.
        unsafe {
            obj::write_ref(info, sfi_name(), name);
            obj::write_ref(info, sfi_type(), desc);
        }
        super::ret_void()
    });
    r.add(W, "fetchStackFrames", "(JJII[Ljava/lang/Object;)I", |t, a| {
        // SAFETY: the anchor is a live `Walk` for the duration of `callStackWalk`.
        let walk = unsafe { &mut *(a[3] as *mut Walk) };
        ret_int(fill(t, a[1], walk, a[5] as i32, a[6] as i32, a[7] as Oop)?)
    });
    // JDK 25: (int mode, long anchor, int lastBatchFrameCount, int bufferSize, int startIndex,
    // frames): fill from startIndex up to the buffer's size.
    r.add(W, "fetchStackFrames", "(IJIII[Ljava/lang/Object;)I", |t, a| {
        // SAFETY: the anchor is a live `Walk` for the duration of `callStackWalk`.
        let walk = unsafe { &mut *(a[2] as *mut Walk) };
        // It returns the number of frames filled (JDK 21's returns the end index).
        let (size, start) = (a[5] as i32, a[6] as i32);
        ret_int(fill(t, a[1] as u32 as u64, walk, size - start, start, a[7] as Oop)? - start)
    });
}

fn is_walker_frame(m: &'static Method) -> bool {
    let n = m.holder.name.as_str();
    n == "java/lang/StackWalker" || n == W || m.holder.super_klass.is_some_and(|s| s.name == W)
}

fn call_stack_walk(t: &mut Thread, walker: Oop, mode: u64, skip: usize, batch: i32, start: i32, buffer: Oop) -> JResult<u64> {
    let show_hidden = mode & SHOW_HIDDEN_FRAMES != 0;
    // Skip this native, the walker's own frames, then `skip` more.
    let frames: Vec<(&'static Method, i32)> = t
        .frames()
        .skip(1)
        .skip_while(|(m, _)| is_walker_frame(m))
        .filter(|(m, _)| show_hidden || !m.is_hidden_frame())
        .skip(skip)
        .collect();
    let mut walk = Walk { frames, next: 0 };
    let anchor = &mut walk as *mut Walk as u64;
    let end = fill(t, mode, &mut walk, batch, start, buffer)?;
    let walker_class = crate::loader::load(W).expect("AbstractStackWalker");
    let do_walk = walker_class
        .declared_method(Symbol::intern("doStackWalk"), Symbol::intern("(JIIII)Ljava/lang/Object;"))
        .expect("doStackWalk");
    // The third argument is the batch size through JDK 21, the number of frames filled since
    // (`crate::jdk::Library::class_frame_info`).
    let third = if crate::jdk::library().class_frame_info() { end - start } else { batch };
    t.call(do_walk, &[walker as u64, anchor, 0, skip as u64, third as u64, start as u64, end as u64])
}

/// Fills `buffer[start..]` with up to `batch` frames; returns the end index.
fn fill(t: &mut Thread, mode: u64, walk: &mut Walk, batch: i32, start: i32, buffer: Oop) -> JResult<i32> {
    // SAFETY: an Object[] (Class[] or StackFrameInfo[]) supplied by the walker.
    let capacity = unsafe { obj::array_length(buffer) };
    let mut i = start;
    while i < capacity && i - start < batch && walk.next < walk.frames.len() {
        let (m, bci) = walk.frames[walk.next];
        walk.next += 1;
        if let (Some(target_off), Some(flags_off)) = (cfi_target(), cfi_flags()) {
            // JDK 25: a pre-allocated ClassFrameInfo (class information only) or StackFrameInfo.
            let info = unsafe { obj::load_elem(buffer, i as usize) };
            let target = if mode & FILL_CLASS_REFS_ONLY != 0 { rt::mirror_of(t, m.holder)? } else { super::mh::resolved_method_name(t, m)? };
            // SAFETY: a live frame object of the walker's buffer; offsets from its layout.
            unsafe {
                let keep = obj::read::<i32>(info, flags_off) & RETAIN_CLASS_REF_BIT;
                obj::write_ref(info, target_off, target);
                obj::write::<i32>(info, flags_off, keep | super::mh::frame_member_flags(m));
                if mode & FILL_CLASS_REFS_ONLY == 0 {
                    obj::write::<i32>(info, sfi_bci(), bci.max(0));
                }
            }
        } else if mode & FILL_CLASS_REFS_ONLY != 0 {
            let mirror = rt::mirror_of(t, m.holder)?;
            // SAFETY: index below the array length.
            unsafe { obj::store_elem(buffer, i as usize, mirror) };
        } else {
            // SAFETY: the element is a StackFrameInfo pre-allocated by the walker.
            let info = unsafe { obj::load_elem(buffer, i as usize) };
            let mn = unsafe { obj::read_ref(info, sfi_member_name()) };
            super::mh::init_member_name(t, mn, m)?;
            unsafe { obj::write::<i32>(info, sfi_bci(), bci.max(0)) };
        }
        i += 1;
    }
    Ok(i)
}
