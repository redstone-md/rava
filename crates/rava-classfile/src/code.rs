//! The `Code` attribute (JVMS 4.7.3): a method's executable body — operand
//! stack/local-slot budget, the raw bytecode, and its exception table. The
//! interpreter consumes this directly.

use crate::reader::Reader;
use crate::ClassError;

#[derive(Debug, Clone)]
pub struct ExceptionHandler {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    /// Constant-pool index of the caught class, or 0 for `finally` (catch-all).
    pub catch_type: u16,
}

#[derive(Debug)]
pub struct Code {
    pub max_stack: u16,
    pub max_locals: u16,
    pub bytecode: Vec<u8>,
    pub exceptions: Vec<ExceptionHandler>,
    /// Interpreter-owned immutable structural classifications. Parsing leaves this zero; the
    /// class loader derives it from bytecode before publishing the class.
    pub structural_flags: u8,
    /// WHERE THE ANSWER TO "can this body run without a frame?" LIVES.
    ///
    /// ⛔**This field exists because of a measured failure.** The interpreter's frame-free path
    /// (`rava_interp::flatcall`) first computed that answer per CALL. For the closed-form shapes
    /// that was a length compare and it measured **−2.69%** on the interpreter gate; extending it
    /// to straight-line bodies made the test WALK the bytecode, and the walk is paid by the
    /// **75% of calls it ends up refusing** — 266 M of them a gate — which dragged the same change
    /// to −0.80% (ledger row 363). A thread-local memo made it worse again.
    ///
    /// A method's bytecode never changes after load, so the answer is a constant of the body. It
    /// belongs next to the body, not in a cache in front of it: one relaxed byte load per call,
    /// and the classification runs once per method for the lifetime of the process.
    ///
    /// The encoding is owned by the interpreter (`0` means "not yet classified"); this crate does
    /// not interpret the other values, it only provides somewhere permanent to put them.
    pub flat_hint: std::sync::atomic::AtomicU8,
    /// The opcode that disqualified this body from the frame-free straight-line executor, or
    /// `0` if it qualified / has not been classified. Written once beside `flat_hint`, read only
    /// by the `RAVA_STRAIGHT_BLOCKERS` census — see `flatcall::straight_blocker`.
    pub flat_blocker: std::sync::atomic::AtomicU8,
    /// Interpreter-owned pointer to immutable compiled-entry metadata. Parsing and cloning leave
    /// this zero; the JIT publishes a process-lifetime record after native code is committed and
    /// clears it before a deopt-triggered replacement. The classfile crate never dereferences it.
    pub jit_entry_hint: std::sync::atomic::AtomicUsize,
}

/// A `Code` with an empty body, so the synthetic bodies the fork compiler and the tests build can
/// spell only the fields they care about and pick up `flat_hint` (and anything added later)
/// automatically. An empty bytecode is never executable, which is the right default for a value
/// that is about to be overwritten field by field.
impl Default for Code {
    fn default() -> Self {
        Self {
            max_stack: 0,
            max_locals: 0,
            bytecode: Vec::new(),
            exceptions: Vec::new(),
            structural_flags: 0,
            flat_hint: std::sync::atomic::AtomicU8::new(0),
            flat_blocker: std::sync::atomic::AtomicU8::new(0),
            jit_entry_hint: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// `Code` is cloned when a class is duplicated; the hint is a pure memo, so a clone starts it over
/// rather than carrying a value that was derived for a different `Code`'s address.
impl Clone for Code {
    fn clone(&self) -> Self {
        Self {
            max_stack: self.max_stack,
            max_locals: self.max_locals,
            bytecode: self.bytecode.clone(),
            exceptions: self.exceptions.clone(),
            structural_flags: self.structural_flags,
            flat_hint: std::sync::atomic::AtomicU8::new(0),
            flat_blocker: std::sync::atomic::AtomicU8::new(0),
            jit_entry_hint: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl Code {
    /// Parse a `Code` attribute body (the reader is positioned just past the
    /// attribute's name+length header). Nested attributes are skipped.
    pub fn parse(r: &mut Reader) -> Result<Self, ClassError> {
        let max_stack = r.u16()?;
        let max_locals = r.u16()?;

        let code_len = r.u32()? as usize;
        let bytecode = r.bytes(code_len)?.to_vec();

        let ex_count = r.u16()?;
        let mut exceptions = Vec::with_capacity(ex_count as usize);
        for _ in 0..ex_count {
            exceptions.push(ExceptionHandler {
                start_pc: r.u16()?,
                end_pc: r.u16()?,
                handler_pc: r.u16()?,
                catch_type: r.u16()?,
            });
        }

        // Nested attributes (LineNumberTable, StackMapTable, ...) — skip bodies.
        let attr_count = r.u16()?;
        for _ in 0..attr_count {
            let _name = r.u16()?;
            let len = r.u32()? as usize;
            r.skip(len)?;
        }

        Ok(Code {
            max_stack,
            max_locals,
            bytecode,
            exceptions,
            structural_flags: 0,
            flat_hint: std::sync::atomic::AtomicU8::new(0),
            flat_blocker: std::sync::atomic::AtomicU8::new(0),
            jit_entry_hint: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}
