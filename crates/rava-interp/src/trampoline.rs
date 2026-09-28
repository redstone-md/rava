//! Explicit-stack (trampolined) execution — TRACER BULLET.
//!
//! The production interpreter (`opcodes::run`) executes a Java call chain on the
//! Rust call stack: an invoke opcode recurses into `Vm::invoke`. That recursion
//! is what blocks checkpoint/restore — a live native call stack can't be
//! serialized, so a booted server can't be frozen and resumed (see the snapshot
//! work in `Heap::snapshot` / `Vm::snapshot`). The fix is an explicit `Vec<Frame>`
//! state machine: a call PUSHES a frame, a return POPS it, and the machine's
//! whole state is plain data — serializable, hence resumable.
//!
//! This module proves the core mechanic (push-on-call, pop-on-return, resume the
//! caller at the instruction AFTER its call, with the callee's result on its
//! stack) on a self-contained integer opcode subset and a synthetic program, so
//! the model is validated before the multi-increment work of trampolining the
//! full opcode set + real constant-pool resolution in `opcodes::run`.

/// A method in the synthetic tracer program: raw bytecode + argument count.
pub struct TMethod {
    pub code: Vec<u8>,
    pub nargs: usize,
    pub max_locals: usize,
}

/// A synthetic program — `invokestatic`'s operand is an index into `methods`.
pub struct TProgram {
    pub methods: Vec<TMethod>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct TFrame {
    mi: usize,
    pc: usize,
    locals: Vec<i32>,
    stack: Vec<i32>,
}

/// The COMPLETE execution state — the frame stack plus the in-flight return
/// value. Because it is plain data (no Rust call frames), it round-trips through
/// `bincode`: you can freeze a half-finished recursion and thaw it later. That is
/// precisely what the recursive `opcodes::run` cannot do, and why this model is
/// the prerequisite for snapshotting a booted server.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct ExecState {
    frames: Vec<TFrame>,
    returned: Option<i32>,
}

impl ExecState {
    fn start(prog: &TProgram, entry: usize, args: Vec<i32>) -> Self {
        ExecState { frames: vec![mk_frame(prog, entry, args)], returned: None }
    }
    /// Freeze the execution state to bytes (a checkpoint).
    pub fn checkpoint(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }
    /// Thaw a checkpoint back into a resumable execution state.
    pub fn restore(bytes: &[u8]) -> Option<ExecState> {
        bincode::deserialize(bytes).ok()
    }
}

fn mk_frame(prog: &TProgram, mi: usize, args: Vec<i32>) -> TFrame {
    let mut locals = args;
    locals.resize(prog.methods[mi].max_locals.max(locals.len()), 0);
    TFrame { mi, pc: 0, locals, stack: Vec::new() }
}

/// Run `prog` from method `entry` with `args`, on an EXPLICIT frame stack (no Rust
/// recursion across Java frames). Returns the entry method's int result, or
/// `None` on an unsupported opcode / malformed program. The entire execution
/// state lives in `frames` — the property that makes checkpoint/restore possible.
pub fn run_trampolined(prog: &TProgram, entry: usize, args: Vec<i32>) -> Option<i32> {
    let mut st = ExecState::start(prog, entry, args);
    if run_budget(prog, &mut st, -1) {
        st.returned
    } else {
        None
    }
}

/// Drive `st` for up to `budget` opcodes (`budget < 0` = run to completion).
/// Returns `true` when execution finished (the frame stack drained — the result
/// is in `st.returned`); `false` when the budget ran out mid-flight, in which
/// case `st` is a complete, checkpointable snapshot to resume later. `None`-style
/// faults (bad opcode / malformed program) also stop with `true` and a cleared
/// state — fine for the tracer.
pub fn run_budget(prog: &TProgram, st: &mut ExecState, mut budget: i64) -> bool {
    loop {
        // Land a callee's return value onto the resumed caller's stack.
        if let Some(r) = st.returned {
            match st.frames.last_mut() {
                Some(f) => {
                    f.stack.push(r);
                    st.returned = None;
                }
                None => return true, // stack drained — `returned` is the result
            }
        }
        if st.frames.is_empty() {
            return true;
        }
        // Run the top frame opcode-by-opcode until it calls, returns, or the
        // budget runs out (each opcode boundary is a clean, checkpointable point).
        loop {
            if budget == 0 {
                return false;
            }
            budget -= 1;
            // Re-borrow the top frame each step so a call/return can mutate the
            // frame stack between opcodes.
            let f = st.frames.last_mut().unwrap();
            let code = &prog.methods[f.mi].code; // borrows prog, not the frame
            let op = match code.get(f.pc) {
                Some(&op) => op,
                None => return true,
            };
            macro_rules! pop {
                () => {
                    match f.stack.pop() {
                        Some(v) => v,
                        None => return true,
                    }
                };
            }
            macro_rules! byte {
                ($o:expr) => {
                    match code.get($o) {
                        Some(&b) => b,
                        None => return true,
                    }
                };
            }
            match op {
                0x03..=0x08 => {
                    f.stack.push(op as i32 - 0x03); // iconst_0..iconst_5
                    f.pc += 1;
                }
                0x10 => {
                    f.stack.push(byte!(f.pc + 1) as i8 as i32); // bipush
                    f.pc += 2;
                }
                0x1a..=0x1d => {
                    let v = match f.locals.get((op - 0x1a) as usize) {
                        Some(&v) => v,
                        None => return true,
                    };
                    f.stack.push(v); // iload_0..iload_3
                    f.pc += 1;
                }
                0x3b..=0x3e => {
                    let v = pop!(); // istore_0..istore_3
                    let i = (op - 0x3b) as usize;
                    if f.locals.len() <= i {
                        f.locals.resize(i + 1, 0);
                    }
                    f.locals[i] = v;
                    f.pc += 1;
                }
                0x60 => {
                    let (b, a) = (pop!(), pop!());
                    f.stack.push(a.wrapping_add(b));
                    f.pc += 1;
                }
                0x64 => {
                    let (b, a) = (pop!(), pop!());
                    f.stack.push(a.wrapping_sub(b));
                    f.pc += 1;
                }
                0x68 => {
                    let (b, a) = (pop!(), pop!());
                    f.stack.push(a.wrapping_mul(b));
                    f.pc += 1;
                }
                0x9e => {
                    let v = pop!(); // ifle (top <= 0)
                    let off = i16::from_be_bytes([byte!(f.pc + 1), byte!(f.pc + 2)]);
                    f.pc = if v <= 0 {
                        (f.pc as i32 + off as i32) as usize
                    } else {
                        f.pc + 3
                    };
                }
                0xa7 => {
                    let off = i16::from_be_bytes([byte!(f.pc + 1), byte!(f.pc + 2)]); // goto
                    f.pc = (f.pc as i32 + off as i32) as usize;
                }
                0xb8 => {
                    // invokestatic <method index> — advance the caller's pc, split
                    // off the args, PUSH the callee. (f's borrow ends after the
                    // split_off, so the push to st.frames is allowed.)
                    let mi = u16::from_be_bytes([byte!(f.pc + 1), byte!(f.pc + 2)]) as usize;
                    f.pc += 3;
                    let nargs = match prog.methods.get(mi) {
                        Some(m) => m.nargs,
                        None => return true,
                    };
                    let at = match f.stack.len().checked_sub(nargs) {
                        Some(a) => a,
                        None => return true,
                    };
                    let cargs = f.stack.split_off(at);
                    let callee = mk_frame(prog, mi, cargs);
                    st.frames.push(callee);
                    break;
                }
                0xac => {
                    let v = pop!(); // ireturn
                    st.frames.pop();
                    st.returned = Some(v);
                    break;
                }
                _ => return true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sum(n) = n<=0 ? 0 : n + sum(n-1), exercised RECURSIVELY through the
    /// explicit frame stack (no Rust recursion across the Java frames).
    fn sum_program() -> TProgram {
        // pc0:  iload_0            (0x1a)
        // pc1:  ifle  +12 -> pc13  (0x9e 00 0c)
        // pc4:  iload_0            (0x1a)        n  (for the n + ...)
        // pc5:  iload_0            (0x1a)        n  (for the n-1 arg)
        // pc6:  iconst_1           (0x04)
        // pc7:  isub               (0x64)        n-1
        // pc8:  invokestatic 0     (0xb8 00 00)  sum(n-1)
        // pc11: iadd               (0x60)        n + sum(n-1)
        // pc12: ireturn            (0xac)
        // pc13: iconst_0           (0x03)
        // pc14: ireturn            (0xac)
        TProgram {
            methods: vec![TMethod {
                code: vec![
                    0x1a, 0x9e, 0x00, 0x0c, 0x1a, 0x1a, 0x04, 0x64, 0xb8, 0x00, 0x00, 0x60,
                    0xac, 0x03, 0xac,
                ],
                nargs: 1,
                max_locals: 1,
            }],
        }
    }

    #[test]
    fn trampoline_runs_recursive_calls_on_an_explicit_frame_stack() {
        let prog = sum_program();
        assert_eq!(run_trampolined(&prog, 0, vec![0]), Some(0));
        assert_eq!(run_trampolined(&prog, 0, vec![1]), Some(1));
        assert_eq!(run_trampolined(&prog, 0, vec![5]), Some(15)); // 5+4+3+2+1+0
        assert_eq!(run_trampolined(&prog, 0, vec![100]), Some(5050));
    }

    #[test]
    fn checkpoint_and_resume_mid_recursion() {
        // THE thesis: freeze a half-finished recursion and resume it correctly —
        // impossible on the Rust call stack, trivial on the explicit one.
        let prog = sum_program();
        let mut st = ExecState::start(&prog, 0, vec![10]);
        let done = run_budget(&prog, &mut st, 25); // stop a few frames deep
        assert!(!done, "should pause mid-flight, not finish");
        assert!(st.frames.len() > 1, "should be several frames deep");

        let frozen = st.checkpoint(); // serialize the live execution state
        assert!(!frozen.is_empty());
        let mut thawed = ExecState::restore(&frozen).expect("restore");
        assert!(run_budget(&prog, &mut thawed, -1), "resume to completion");
        assert_eq!(thawed.returned, Some(55)); // sum(10) = 55, computed across the freeze
    }

    #[test]
    fn trampoline_two_method_call_chain() {
        // m0: return 7 + m1(); m1: return 35.
        let prog = TProgram {
            methods: vec![
                TMethod {
                    // bipush 7; invokestatic 1; iadd; ireturn
                    code: vec![0x10, 0x07, 0xb8, 0x00, 0x01, 0x60, 0xac],
                    nargs: 0,
                    max_locals: 0,
                },
                TMethod {
                    // bipush 35; ireturn
                    code: vec![0x10, 0x23, 0xac],
                    nargs: 0,
                    max_locals: 0,
                },
            ],
        };
        assert_eq!(run_trampolined(&prog, 0, vec![]), Some(42));
    }
}
