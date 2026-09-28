//! Lambda objects: their site, captures, header and direct-call eligibility.

use crate::heap::*;

impl Heap {
    /// Allocate a functional-interface closure; returns its handle.
    pub fn alloc_lambda(&mut self, lambda: Lambda) -> u32 {
        let class = self.intern("$Lambda");
        self.bytes.fetch_add(48 + lambda.captured_bytes(), std::sync::atomic::Ordering::Relaxed);
        self.put_object(Object { class, fields: FieldBlob::new(), lambda: Some(Box::new(lambda)) })
    }

    /// Interned call-site identity of a lambda WITHOUT cloning its captures
    /// (probe/diagnostic use: `Arc` bump only).
    pub fn lambda_site_of(&self, handle: u32) -> Option<Arc<LambdaSite>> {
        self.get(handle)
            .and_then(|o| o.lambda.as_ref().map(|l| Arc::clone(&l.site)))
    }

    /// IMPLEMENTATION-IDENTITY key of a lambda receiver (0 = not a live lambda).
    ///
    /// The `Arc<LambdaSite>` a closure carries is MINTED ONCE per `invokedynamic`
    /// instruction (`opcodes::invokedynamic` → `Plan::Lambda`, cached per site in
    /// `tlcache::indy_put`) and CLONED into every instantiation, so its address is:
    ///  • INVARIANT across all closures born at one indy site — which is exactly the
    ///    property the receiver-object identity lacks for a CAPTURING lambda, and the
    ///    reason such a site can never go monomorphic on a receiver key;
    ///  • DISTINCT per indy site (each site does its own `Arc::new`), hence it implies
    ///    (owner, name, descriptor) AND the MethodHandle `kind` AND the capture arity
    ///    (the indy descriptor's parameter count is a constant of the instruction) —
    ///    i.e. everything `jit_direct_resolve` derives except the captured VALUES,
    ///    which are re-read from the receiver on every call;
    ///  • READ WITH THREE DEPENDENT LOADS from the receiver cell (slot → `Object` box →
    ///    `Lambda` box → `site` word), one more than the class word the heap-Ref PIC
    ///    already reads.
    ///
    /// Bit 63 is set so the key is TOTALLY DISJOINT from every other receiver-guard
    /// kind sharing the direct-call cache's key space: interned class-name pointers and
    /// arena class-id+1 are small, and a lambda identity cell is `REF_TAG|handle` (bit
    /// 40). Arc payloads live far below 2^63, so the tag is lossless.
    ///
    /// LIFETIME: callers that PERSIST this key must first pin the `Arc` through
    /// `nextgen::lambda_site_pin`, otherwise a dropped site could be freed and its
    /// address recycled by an unrelated site — the one way this key could alias.
    pub fn lambda_site_key(&self, handle: u32) -> u64 {
        match self.get(handle)
            .and_then(|o| o.lambda.as_deref())
        {
            Some(l) => (Arc::as_ptr(&l.site) as u64) | (1u64 << 63),
            None => 0,
        }
    }

    /// Borrow a lambda's captured values in place — the per-call read the direct
    /// call path needs. `lambda_of` CLONES the whole `Lambda` (a fresh `Vec<Value>`
    /// allocation on every capturing-lambda dispatch); this returns the live slice.
    pub fn lambda_captures(&self, handle: u32) -> Option<&[Value]> {
        self.get(handle)
            .and_then(|o| o.lambda.as_deref())
            .map(|l| l.captured.as_slice())
    }

    /// Borrow a lambda's whole DISPATCH HEADER in place: `(kind, &site, &captures)`.
    /// The apply path needs exactly these three and nothing else, and it needs them
    /// together — reading them through `lambda_of` costs a full `Lambda` clone (a
    /// `Vec<Value>` allocation) purely to carry a `u8` and an `Arc` bump across the
    /// borrow. This is the same "borrow, don't snapshot" treatment `lambda_captures`
    /// already gives the direct-call path, in ONE slot walk instead of three.
    ///
    /// GC-SAFETY: the returned borrows keep `&self` alive, so the caller physically
    /// cannot reach a `&mut self` collection point while holding them — the copy out
    /// (`Arc::clone` + `extend_from_slice`) is forced to complete first, and borrowck
    /// proves it. A borrow can never span a collection here.
    pub fn lambda_header(&self, handle: u32) -> Option<(u8, &Arc<LambdaSite>, &[Value])> {
        self.get(handle)
            .and_then(|o| o.lambda.as_deref())
            .map(|l| (l.kind, &l.site, l.captured.as_slice()))
    }

    pub fn lambda_of(&self, handle: u32) -> Option<Lambda> {
        // CONSUMER ACCOUNTING (RAVA_GC_STATS): the handle representation's snapshot
        // cost, measured with the same clock as the arena one so an A/B of the two is
        // like-for-like rather than a wall-delta inference.
        #[cfg(feature = "next-gen-gc")]
        let _t0 = crate::nextgen::gc_stats_enabled().then(std::time::Instant::now);
        let out = self.get(handle)
            .and_then(|o| o.lambda.as_deref().cloned());
        #[cfg(feature = "next-gen-gc")]
        if let Some(t0) = _t0 {
            if out.is_some() {
                crate::nextgen::gc_stats_note_snapshot(false, t0.elapsed().as_nanos() as u64);
            }
        }
        out
    }

    /// Whether `handle` is a lambda/method-reference object — answered without
    /// cloning the `Lambda` (the hot invokevirtual path only needs existence;
    /// cloning it per virtual call was a top allocator source).
    pub fn is_lambda(&self, handle: u32) -> bool {
        self.get(handle)
            .is_some_and(|o| o.lambda.is_some())
    }

    /// Whether a lambda can take the identity-guarded direct-call path: a
    /// NON-CAPTURING static/unbound method ref (these are per-indy-site
    /// singletons, so an identity guard is stable and the PIC stays small).
    /// Capturing lambdas are fresh objects per closure — identity-guarding
    /// them would mint a negative-cache entry per INSTANCE. No cloning.
    pub fn lambda_directable(&self, handle: u32) -> bool {
        self.lambda_directable_why(handle) == 0
    }

    /// WHY a receiver is not λ-PIC-able: `0` = it is. The λ-PIC's own census
    /// (`RAVA_JIT_PIC_PROBE`) reads **`not_lambda_directable = 22,084,340`, `ADMITTED = 0`** over
    /// a boot, so a paired A/B of `RAVA_LAMBDA_PIC` (−0.18%, arms overlapping) measured a
    /// mechanism that never fired. That count is only actionable once it separates "the receiver
    /// is not a lambda at all" — a correct, cheap skip — from "it is a lambda this gate refuses",
    /// which is a population with a possible mechanism behind it.
    ///
    /// 1 = not a lambda · 2 = CAPTURING · 3 = kind outside {5,6,9}.
    pub fn lambda_directable_why(&self, handle: u32) -> u8 {
        let Some(l) = self.get(handle)
            .and_then(|o| o.lambda.as_deref())
        else {
            return 1;
        };
        if !l.captured.is_empty() {
            return 2;
        }
        if !matches!(l.kind, 5 | 6 | 9) {
            return 3;
        }
        0
    }

    /// Content fingerprint of a lambda (0 = not a lambda). The direct-call cache
    /// guards lambda receivers by handle IDENTITY; a swept handle can be REUSED
    /// by a different lambda, so the cached entry re-checks this fingerprint
    /// (kind + owner + name + descriptor + captured count) before trusting the
    /// identity match. No cloning, and — since `LambdaSite::fp` pins the
    /// owner/name/descriptor fold at construction — no per-call string walk
    /// either: only the two PER-INSTANCE discriminators (`kind`, capture arity)
    /// are mixed here. Same inputs, same discrimination, O(1) instead of
    /// O(len(owner)+len(name)+len(descriptor)) on every dispatch.
    pub fn lambda_fp(&self, handle: u32) -> u64 {
        let Some(l) = self.get(handle)
            .and_then(|o| o.lambda.as_deref())
        else {
            return 0;
        };
        let mut h = l.site.fp;
        h = (h.rotate_left(5) ^ l.kind as u64).wrapping_mul(0x100_0000_01B3);
        h = (h.rotate_left(5) ^ l.captured.len() as u64).wrapping_mul(0x100_0000_01B3);
        h | 1 // never 0 for a real lambda
    }
}
