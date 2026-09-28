//! getstatic/putstatic and the allocation opcodes served from the interpreter.

use crate::*;

impl Vm {
    /// getstatic dispatch from the rava_getstatic callback. Drives the REAL
    /// `opcodes::getstatic` through a synthetic frame (caller class for the cp
    /// lookup), so the System.out sentinel / super + interface-constant walk /
    /// native fallback stay intact. Packs per the field kind (ref → box_ref).
    pub(crate) fn jit_getstatic(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        is_ref: bool,
    ) -> Result<i64, VmError> {
        // Warm-path probe BEFORE any Arc juggling or synthetic-Frame build: the
        // tlcache site key is (class ptr, cp) — exactly `caller_ptr` — so a hit
        // packs and returns without the strong-count round-trip, the Frame
        // construct+drop, and the interpreter handler's `ref_parts`.
        if let Some(v) = tlcache::getstatic_get((caller_ptr as usize, cp), self.runtime_shared().static_gen()) {
            return Ok(if is_ref { jit::box_ref(self, &v) } else { jit::pack_local(&v) });
        }
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        let frame = Frame::from_parts(std::ptr::null(), Vec::new(), 0, caller);
        let v = opcodes::getstatic(self, &frame, cp)?;
        Ok(if is_ref {
            jit::box_ref(self, &v)
        } else {
            jit::pack_local(&v)
        })
    }

    /// putstatic from compiled code: drive the real interpreter handler (runs the
    /// owner `<clinit>`, walks the inherited-static chain) with `value` on a
    /// synthetic frame's stack, so it cannot diverge from the interpreter.
    pub(crate) fn jit_putstatic(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        value: Value,
    ) -> Result<(), VmError> {
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        let mut frame = Frame {
            pc: 0,
            // Synthetic, as above.
            code: std::ptr::null(),
            buf: vec![value],
            nlocals: 0,
            class: caller,
        };
        opcodes::putstatic(self, &mut frame, cp)
    }

    /// `new` from compiled code: drive the real interpreter handler (runs the
    /// target class `<clinit>`, then allocates). Returns the tagged reference cell
    /// for the fresh object. The handle is created last (no GC safepoint between
    /// alloc and the compiled code rooting it in an operand cell).
    pub(crate) fn jit_new(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
    ) -> Result<i64, VmError> {
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        // M5.4 PROBE (RAVA_NEWCB_SCAN, inert): classify this slow FFI-new callback — FIXABLE = the class
        // is NOW initialised + not enum + not native-backed, so it would be inline-arena-eligible if the
        // method were (re)compiled now (agy's Rank-3 lever: runtime init-guard → inline TLAB bump).
        // UNFIXABLE = enum / native-backed (permanently a handle). Measures the lever's real ceiling.
        #[cfg(feature = "next-gen-gc")]
        {
            static NEWCB_SCAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *NEWCB_SCAN.get_or_init(|| std::env::var_os("RAVA_NEWCB_SCAN").is_some()) {
                if let Some(name) = caller.file.pool.class_name(cp).map(str::to_string) {
                    self.newcb_classify(&name);
                }
            }
        }
        let frame = Frame::from_parts(std::ptr::null(), Vec::new(), 0, caller);
        // TASK 2: compiled `new` allocates through the ARENA path under the flag
        // (a pure-bytecode class becomes a Value::Obj; native-backed/enum stay
        // handles), packed via box_ref (raw ArenaPtr / tagged handle). Nursery-full
        // self-collects (new_object_value → arena_collect_jit). Default build is
        // byte-identical (the legacy handle alloc).
        #[cfg(feature = "next-gen-gc")]
        {
            match opcodes::new_object_value(self, &frame.class, cp)? {
                opcodes::NewSignal::Value(v) => Ok(jit::box_ref(self, &v)),
                // Recursive/compiled path never signals these: nursery-full
                // collects+retries inside new_object_value, clinit inits inline.
                opcodes::NewSignal::NurseryFull => {
                    Ok(jit::box_ref(self, &Value::Null))
                }
                opcodes::NewSignal::ClinitPending(_) => {
                    unreachable!("recursive new_object_value inits clinit inline")
                }
            }
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let h = opcodes::new_object(self, &frame, cp)?;
            Ok(jit::pack_local(&Value::Ref(h)))
        }
    }

    /// M5.4 PROBE: classify a slow FFI-new callback by whether agy's Rank-3 lever could convert it to
    /// an inline TLAB bump. Mirrors arena_new_site's eligibility at RUNTIME: FIXABLE = now-initialised +
    /// not enum + not native-backed (would be inline if recompiled now — the lever target). enum/native
    /// = permanently a handle (unfixable). Measures the lever's real ceiling before building it.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn newcb_classify(&mut self, name: &str) {
        use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
        static TOTAL: AtomicU64 = AtomicU64::new(0);
        static FIXABLE: AtomicU64 = AtomicU64::new(0);
        static ENUM: AtomicU64 = AtomicU64::new(0);
        static NATIVE: AtomicU64 = AtomicU64::new(0);
        static UNINIT: AtomicU64 = AtomicU64::new(0);
        let (mut fx, mut en, mut na, mut ui) = (0u64, 0u64, 0u64, 0u64);
        if crate::opcodes::is_native_backed_alloc(name) {
            na = 1;
        } else if let Ok(loaded) = self.loader.get_or_load(name) {
            let is_enum = loaded.file.access_flags & 0x4000 != 0
                || loaded.file.super_class.as_deref() == Some("java/lang/Enum");
            let initialized = loaded.initialized.load(std::sync::atomic::Ordering::Relaxed);
            drop(loaded);
            let native_super =
                self.has_native_backed_ancestor(name) && !self.overrides_collection_backing(name);
            if is_enum {
                en = 1;
            } else if native_super {
                na = 1;
            } else if !initialized {
                ui = 1;
            } else {
                fx = 1;
            }
        }
        FIXABLE.fetch_add(fx, Relaxed);
        ENUM.fetch_add(en, Relaxed);
        NATIVE.fetch_add(na, Relaxed);
        UNINIT.fetch_add(ui, Relaxed);
        let t = TOTAL.fetch_add(1, Relaxed) + 1;
        if t % 1_000_000 == 0 {
            eprintln!(
                "[NEWCB] total={t} FIXABLE(now-init arena-eligible)={} enum={} native={} still-uninit={}",
                FIXABLE.load(Relaxed), ENUM.load(Relaxed), NATIVE.load(Relaxed), UNINIT.load(Relaxed)
            );
        }
    }

    /// `instanceof` from compiled code: resolve the target class name from the
    /// caller pool, decode the reference, and run the real interpreter test.
    /// Returns 1/0.
    pub(crate) fn jit_instanceof(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        val: i64,
    ) -> Result<i64, VmError> {
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        let value = jit::unbox_ref(self, val, "");
        // Site-keyed instanceof (see `opcodes::instance_of_at`). The synthetic
        // Frame this used to build existed only to reach the caller's constant
        // pool for the target name; the site form borrows it directly, so the
        // target `String` is gone from the miss path and the whole hierarchy walk
        // from the hit path.
        Ok(opcodes::instance_of_at(self, &caller, cp, &value)? as i64)
    }

    /// `anewarray` from compiled code: resolve the element class from the caller
    /// pool and allocate a reference array of `count`. Returns the tagged array
    /// cell (handle minted last, no GC safepoint before the compiled code roots it).
    pub(crate) fn jit_anewarray(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        count: usize,
    ) -> Result<i64, VmError> {
        let caller: Arc<LoadedClass> = unsafe {
            Arc::increment_strong_count(caller_ptr);
            Arc::from_raw(caller_ptr)
        };
        // RAVA_ANEWARRAY_SCRATCH: borrow the element name from the constant pool
        // and build `L<name>;` in a stack buffer — the historic arm below paid
        // TWO heap Strings (`class_name_at`'s to_string + the format!) on every
        // compiled-code array allocation, a 2.3-2.9%-of-samples callback.
        // Only taken when the ROUTE MEMO is off: with both flags on, the memo's
        // per-site id replay in anewarray_ref is strictly cheaper than any
        // descriptor build, and the scratch arm is layered on its miss path —
        // an early return here would silently disable that composition.
        if crate::anewarray_scratch() && !opcodes::alloc_route_memo_enabled() {
            let elem = caller.file.pool.class_name(cp).ok_or(VmError::BadConstant(cp))?;
            crate::heap::array_intern_stats::count_ref_desc_built();
            let h = opcodes::with_ref_desc(elem, |d| self.heap.alloc_array(d, count));
            return Ok(jit::pack_local(&Value::Array(h)));
        }
        let frame = Frame::from_parts(std::ptr::null(), Vec::new(), 0, caller);
        let h = opcodes::anewarray_ref(self, &frame.class, cp, count)?;
        Ok(jit::pack_local(&Value::Array(h)))
    }
}
