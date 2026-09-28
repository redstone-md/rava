//! Object shape: new, checkcast, instanceof, array creation and length.

use super::{guard, throw_exc};
use crate::opcodes::*;

/// `None` = keep interpreting, `Some(exit)` = leave `run`.
#[inline(always)]
/// ⚠**`#[inline(never)]` DELIBERATELY** — row 397: inlined into `opcodes::run`, this body's
/// locals are reserved by `run`'s prologue, which every interpreted call pays. `run` measured
/// a 2136-byte frame; the frame itself costs 160 ns against HotSpot's 7.2.
#[inline(never)]
pub(crate) fn objects(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    code: &Code,
    bc: &[u8],
    pc: usize,
    op: u8,
) -> Result<Option<RunExit>, VmError> {
    let _ = (bc, pc, op);
    match op {
        0xbc => { // newarray (primitive)
            let elem = primitive_array_type(bc[pc + 1]);
            let len = frame.pop_int()?;
            // JVMS 6.5.newarray: a negative count is NegativeArraySizeException,
            // whose message is the count. Widening straight to usize turned -1
            // into 2^64-1 and killed the PROCESS in the allocator ("capacity
            // overflow") — a guest `catch (NegativeArraySizeException)` never ran.
            // The compiled path already tested this (jit/callbacks/arrays.rs).
            if len < 0 {
                throw_exc!(vm, frame, code, vm.make_exception_shared(
                    "java/lang/NegativeArraySizeException", &len.to_string()));
            }
            let h = vm.heap.alloc_array(elem, len as usize);
            frame.push(Value::Array(h));
            frame.pc += 2;
        }
        // monitorenter/monitorexit. The NULL CHECK is not optional: JVMS 6.5 says monitorenter
        // on null throws NullPointerException, and a guest that synchronizes on a field it
        // forgot to initialise gets silence here where every JVM gives it an exception.
        // Measured against `java -Xint` in bench/NpeMsg.java, which is also where the message's
        // exact wording comes from.
        //
        // ⭐**AND THE LOCK IS REAL NOW** (`monitor.rs`). It was a no-op for as long as one thread
        // ran bytecode — sound then, and silently wrong the moment the GIL hand-off let a worker
        // run guest code: Guava's `Suppliers.memoize` double-checked lock admitted two threads
        // into its critical section and Minecraft's `Codec$RecursiveCodec` got a `DataResult`
        // back where a `Codec` belonged. `monitor::enter` returns immediately while no real
        // thread exists, so the single-threaded engine still pays one relaxed load.
        0xc2 | 0xc3 => {
            let v = frame.pop()?;
            if matches!(v, Value::Null) {
                let src = crate::npe::source_at(&code.bytecode, frame.pc, 0, &frame.class.file.pool);
                let msg = crate::npe::monitor_null(src);
                throw_exc!(vm, frame, code, vm.make_exception_shared("java/lang/NullPointerException", &msg));
            }
            if let Some(k) = crate::monitor::key_of(&v) {
                if op == 0xc2 { crate::monitor::enter(k) } else { crate::monitor::exit(k) }
            }
            frame.pc += 1;
        }
        0xc0 => { // checkcast
            // The receiver STAYS on the stack (checkcast neither pops nor pushes
            // on success), so peek rather than pop — a throw must leave the
            // operand stack exactly as an exception handler expects to find it.
            //
            // Null passes unconditionally (JVMS 6.5 checkcast), which is why the
            // null case is decided HERE and not inside the type test: for
            // `instanceof` null is a plain false, for a cast it is a pass.
            //
            // `Tri::Unknown` — the walk could not reach the JDK boundary — also
            // passes. That is the whole reason this was a no-op before: a lenient
            // `bool` cannot tell "provably wrong type" from "cannot see far
            // enough", and throwing on the latter invents ClassCastExceptions.
            let peek = match frame.stack().last() {
                Some(Value::Null) | None => None,
                Some(v) => Some(v.clone()),
            };
            if let Some(v) = peek {
                let cp = u16_operand(bc, pc);
                if let Some(msg) = checkcast_reject(vm, &frame.class, cp, &v)? {
                    // pc is NOT advanced first: the exception is raised AT the
                    // checkcast, so the handler search must see the cast's own pc.
                    throw_exc!(vm, frame, code, vm.make_exception_shared("java/lang/ClassCastException", &msg));
                }
            }
            frame.pc += 3;
        }
        0xc1 => { // instanceof
            let cp = u16_operand(bc, pc);
            let v = frame.pop()?;
            // Site-keyed (see `instance_of_at`): the target comes from the
            // caller's constant pool by reference, so even a MISS no longer
            // allocates a `String` for it.
            let yes = instance_of_at(vm, &frame.class, cp, &v)?;
            frame.push(Value::Int(yes as i32));
            frame.pc += 3;
        }
        0xbd => { // anewarray (reference)
            // Both allocation levers route through anewarray_ref: the
            // ALLOC_ROUTE_MEMO site cache is its fast path, and the
            // ANEWARRAY_SCRATCH stack-buffer descriptor is its miss path -
            // layered there so the two flags compose instead of forking
            // this opcode arm.
            let len = frame.pop_int()?;
            // Same negative-count contract as newarray above (JVMS 6.5.anewarray).
            if len < 0 {
                throw_exc!(vm, frame, code, vm.make_exception_shared(
                    "java/lang/NegativeArraySizeException", &len.to_string()));
            }
            let h = anewarray_ref(vm, &frame.class, u16_operand(bc, pc), len as usize)?;
            frame.push(Value::Array(h));
            frame.pc += 3;
        }
        0xbe => { // arraylength
            let len = match frame.pop()? {
                Value::Array(h) => vm.heap.array_len(h).unwrap_or(0),
                #[cfg(feature = "next-gen-gc")]
                Value::Obj(pointer) if crate::nextgen::is_arena_ref_array(pointer) => {
                    crate::heap::ArrayRef::Arena(pointer).len(&vm.heap).unwrap_or(0)
                }
                Value::Null => {
                    if crate::env_flag!("RAVA_TRACE_ARRLEN") {
                        eprintln!("[ARRLEN-NULL] {} pc={}\n{}", frame.class.file.this_class, frame.pc, vm.frame_trace());
                    }
                    {
                        // Nothing sits above the array reference for arraylength.
                        let src = crate::npe::source_at(
                            &code.bytecode, frame.pc, 0, &frame.class.file.pool);
                        let msg = crate::npe::array_length(src);
                        throw_exc!(vm, frame, code, vm.make_exception_shared("java/lang/NullPointerException", &msg));
                    }
                }
                other => {
                    if crate::env_flag!("RAVA_TRACE_UNDERFLOW") {
                        eprintln!("[BADARRLEN] {} pc={} arraylength on non-array {:?}", frame.class.file.this_class, frame.pc, other);
                    }
                    return Err(VmError::StackUnderflow);
                }
            };
            frame.push(Value::Int(len as i32));
            frame.pc += 1;
        }

        // stack manipulation (long/double are single-slot in this model)
        _ => return Err(VmError::UnknownOpcode(op)),
    }
    Ok(None)
}
