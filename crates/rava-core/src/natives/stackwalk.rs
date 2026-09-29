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
        call_stack_walk,
    );
    r.add(W, "fetchStackFrames", "(JJII[Ljava/lang/Object;)I", |t, a| {
        // SAFETY: the anchor is a live `Walk` for the duration of `callStackWalk`.
        let walk = unsafe { &mut *(a[3] as *mut Walk) };
        ret_int(fill(t, a[1], walk, a[5] as i32, a[6] as i32, a[7] as Oop)?)
    });
}

fn is_walker_frame(m: &'static Method) -> bool {
    let n = m.holder.name.as_str();
    n == "java/lang/StackWalker" || n == W || m.holder.super_klass.is_some_and(|s| s.name == W)
}

fn call_stack_walk(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (walker, mode, skip) = (a[0] as Oop, a[1], a[3] as i32 as usize);
    let (batch, start, buffer) = (a[6] as i32, a[7] as i32, a[8] as Oop);
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
    t.call(do_walk, &[walker as u64, anchor, 0, skip as u64, batch as u64, start as u64, end as u64])
}

/// Fills `buffer[start..]` with up to `batch` frames; returns the end index.
fn fill(t: &mut Thread, mode: u64, walk: &mut Walk, batch: i32, start: i32, buffer: Oop) -> JResult<i32> {
    // SAFETY: an Object[] (Class[] or StackFrameInfo[]) supplied by the walker.
    let capacity = unsafe { obj::array_length(buffer) };
    let mut i = start;
    while i < capacity && i - start < batch && walk.next < walk.frames.len() {
        let (m, bci) = walk.frames[walk.next];
        walk.next += 1;
        if mode & FILL_CLASS_REFS_ONLY != 0 {
            let mirror = rt::mirror_of(t, m.holder)?;
            // SAFETY: index below the array length.
            unsafe { *obj::array_data::<Oop>(buffer).add(i as usize) = mirror };
        } else {
            // SAFETY: the element is a StackFrameInfo pre-allocated by the walker.
            let info = unsafe { *obj::array_data::<Oop>(buffer).add(i as usize) };
            let mn = unsafe { obj::read::<Oop>(info, sfi_member_name()) };
            super::mh::init_member_name(t, mn, m)?;
            unsafe { obj::write::<i32>(info, sfi_bci(), bci.max(0)) };
        }
        i += 1;
    }
    Ok(i)
}
