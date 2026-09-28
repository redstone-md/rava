//! String and StringBuilder backing storage.

use crate::heap::*;

impl Heap {
    /// Build a `String` from a char-array (code-point `Int` elements), reading
    /// the backing `Vec<i32>` directly — no intermediate `Vec<Value>`.
    pub fn chars_array_to_string(&self, arr: u32) -> String {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        match arrays.get(arr as usize).map(|a| &a.data) {
            Some(ArrayData::Int(v)) => v.iter().filter_map(|&c| char::from_u32(c as u32)).collect(),
            _ => self
                .array_values(arr)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32))
                .collect(),
        }
    }

    /// A RANGE of a char-array as a `String`, reading the backing `Vec<i32>` directly.
    ///
    /// ⭐`StringBuilder.append(char[], off, len)` built a boxed `Vec<Value>` of the WHOLE array
    /// and a `Vec<char>` from it, then threw away everything outside `off..off+len`. gson's
    /// reader appends out of an 8192-char refill buffer, so a 780-byte file boxed 8192 values to
    /// keep 780 of them.
    ///
    /// It also applied `off`/`len` to the sequence AFTER dropping code points `char::from_u32`
    /// rejects (unpaired surrogates), which shifts every index past one. The JVM slices the RAW
    /// array, and so does this.
    pub fn chars_slice_to_string(&self, arr: u32, off: usize, len: usize) -> String {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        match arrays.get(arr as usize).map(|a| &a.data) {
            Some(ArrayData::Int(v)) => {
                let end = off.saturating_add(len).min(v.len());
                if off >= end {
                    return String::new();
                }
                v[off..end].iter().filter_map(|&c| char::from_u32(c as u32)).collect()
            }
            _ => {
                let vals = self.array_values(arr).unwrap_or_default();
                let end = off.saturating_add(len).min(vals.len());
                if off >= end {
                    return String::new();
                }
                vals[off..end]
                    .iter()
                    .filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32))
                    .collect()
            }
        }
    }

    /// Widen `bytes` into a char-array `dst` starting at `off`, each as an UNSIGNED byte —
    /// exactly what the per-byte reader did. Returns how many were written.
    ///
    /// ⭐**This replaces a loop that cost four hashed or boxed operations PER BYTE.**
    /// `Reader.read(char[])` drained its stream one byte at a time, and each byte paid a
    /// `get_field(h, "$pushback")` (a hash of the field NAME), a stream-state walk, a boxed
    /// `array_get`, and a `put_field_syn` to advance the position. Reading 5 MB of datapack JSON
    /// that way took **1,099 ms — 220 ns per byte** — while `Files.readAllBytes` over the same
    /// files took 137 ms, so neither the archive nor the decompressor was the problem: the cost
    /// was entirely in moving bytes across the boundary one at a time.
    ///
    /// `DataInputStream` was given exactly this treatment already (`RAVA_NO_BULK_READ`, "six heap
    /// operations to move one byte", 3.1× on the save gate). `Reader.read(char[])` is the same
    /// loop and was missed — the second sibling-accessor miss found in this file today.
    ///
    /// Semantics unchanged on purpose: a byte becomes a char with NO UTF-8 decoding, which is
    /// what the old loop did. (`read(CharBuffer)` decodes instead — a real inconsistency, but a
    /// separate question from making this bulk.)
    pub fn array_write_chars(&mut self, dst: u32, off: usize, bytes: &[u8]) -> usize {
        {
            let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
            if let Some(ArrayData::Int(d)) = arrays.get_mut(dst as usize).map(|a| &mut a.data) {
                return widen_run(&mut d[..], off, bytes);
            }
        }
        let mut n = 0;
        for &b in bytes {
            if self.array_get(dst, off + n).is_none() {
                break;
            }
            self.array_set(dst, off + n, Value::Int(b as i32));
            n += 1;
        }
        n
    }

    /// **The fused form of `array_read_bytes` + `array_write_chars`: widen a run of a BYTE array
    /// straight into a CHAR array, with NOTHING allocated between them.**
    ///
    /// ⭐`Reader.read(char[],off,len)` was already bulk, but it was bulk in TWO steps: the source
    /// run was first copied out of the heap into a fresh `Vec<u8>` (`array_read_bytes` ends in
    /// `to_vec()`), and only then widened into the destination. gson's `JsonReader` refills a
    /// `char[1024]` per fill, so that transient buffer was one malloc + one 1 KB memcpy per refill
    /// of every file read through a `Reader` — a whole extra copy of the text, of a run that never
    /// escapes the call. Both arrays live in `self.arrays`, so a `split_at_mut` reaches them at
    /// once and the byte→i32 widening reads the source in place.
    ///
    /// Returns `None` — never a partial or wrong answer — for every shape it cannot prove:
    /// * `src == dst` (the two runs could overlap; `split_at_mut` cannot even name them),
    /// * either handle out of range,
    /// * a source that is not `ArrayData::Byte` or a destination that is not `ArrayData::Int`.
    ///
    /// A `None` costs the caller nothing but the old two-step, which it then runs verbatim, so the
    /// guard can only ever REJECT. `Some(0)` is the end-of-input answer and means exactly what
    /// `array_read_bytes`' empty slice meant: the source position is at or past the source length.
    ///
    /// No allocation ⇒ no safepoint ⇒ nothing here can move an object out from under the two
    /// slices, which is why holding both across the copy is sound.
    pub fn array_widen_bytes_to_chars(
        &mut self,
        src: u32,
        spos: usize,
        dst: u32,
        doff: usize,
        want: usize,
    ) -> Option<usize> {
        let (si, di) = (src as usize, dst as usize);
        if si == di {
            return None;
        }
        {
            let count = self.arrays.read().unwrap_or_else(|e| e.into_inner()).len();
            if si.max(di) >= count {
                return None;
            }
        }
        // Disjoint mutable halves either way round; the source half is reborrowed shared.
        let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        if si < di {
            let (head, tail) = arrays.split_at_mut(di);
            match (&head[si].data, &mut tail[0].data) {
                (ArrayData::Byte(s), ArrayData::Int(d)) => Some(widen_slice(s, spos, d, doff, want)),
                _ => None,
            }
        } else {
            let (head, tail) = arrays.split_at_mut(si);
            match (&tail[0].data, &mut head[di].data) {
                (ArrayData::Byte(s), ArrayData::Int(d)) => Some(widen_slice(s, spos, d, doff, want)),
                _ => None,
            }
        }
    }

    /// Read an object's `$sb` text (handles both the char-array backing and a
    /// legacy/uninitialised `Value::Str`).
    pub fn sb_read(&self, handle: u32) -> String {
        match self.get_field_syn(handle, crate::heap::Syn::Sb) {
            Value::Array(a) => self.chars_array_to_string(a),
            Value::Str(s) => s.to_string(),
            _ => String::new(),
        }
    }

    /// The `$sb` buffer's LENGTH, without materialising it.
    ///
    /// ⭐`sb_read` builds a whole `String` — it walks the backing `Vec<i32>`, decodes every code
    /// point and allocates. `StringBuilder.length()` went through it, so a length query cost
    /// O(n) plus a malloc, and the standard drain loop
    /// `while (sb.length() < n) sb.append(...)` was O(n²). The length is the array's length.
    pub fn sb_len(&self, handle: u32) -> usize {
        match self.get_field_syn(handle, crate::heap::Syn::Sb) {
            Value::Array(a) => {
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                match arrays.get(a as usize).map(|x| &x.data) {
                    Some(ArrayData::Int(v)) => v.len(),
                    _ => self.array_values(a).map(|v| v.len()).unwrap_or(0),
                }
            }
            // A `Str` backing is a char count, not a byte count — ASCII is the common case and
            // `chars().count()` is only reached for text that is not.
            Value::Str(s) => {
                if s.is_ascii() {
                    s.len()
                } else {
                    s.chars().count()
                }
            }
            _ => 0,
        }
    }

    /// One character of the `$sb` buffer, without materialising it. `None` = out of range, which
    /// the caller turns into the same exception the JVM raises. Twin of `sb_len`.
    pub fn sb_char_at(&self, handle: u32, i: usize) -> Option<i32> {
        match self.get_field_syn(handle, crate::heap::Syn::Sb) {
            Value::Array(a) => {
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                match arrays.get(a as usize).map(|x| &x.data) {
                    Some(ArrayData::Int(v)) => v.get(i).copied(),
                    _ => self.array_values(a).and_then(|v| v.get(i).and_then(|x| x.as_int())),
                }
            }
            Value::Str(s) => {
                let b = s.as_bytes();
                if i < b.len() && b[..=i].is_ascii() {
                    Some(b[i] as i32)
                } else {
                    s.chars().nth(i).map(|c| c as i32)
                }
            }
            _ => None,
        }
    }

    /// Append `text` to an object's `$sb` buffer in place (amortized O(1)),
    /// creating the char-array on first use. Extends the backing `Vec<i32>` in
    /// one shot with a SINGLE descriptor resync (per-char `array_push` would
    /// resync every char — an O(n) tax that made short appends slower than the
    /// old immutable-Str backing).
    pub fn sb_append(&mut self, handle: u32, text: &str) {
        if text.is_empty() {
            return;
        }
        // BY ID, NOT BY NAME. `get_field` hashes the field NAME and probes the interner on every
        // call, and `Appendable.append` is 1.83% of the boot's 64,088,346 compiled-to-host
        // crossings. The same shape memoised in `DoubleArrayList.wrap` took it from 3,410 ns to
        // 600. `field_id_opt` is cached only once RESOLVED — caching a `None` would disable the
        // fast read forever if the first call arrived before the name was interned.
        static SB_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let sb_id = match SB_ID.get() {
            Some(&id) => Some(id),
            None => match self.field_id_opt("$sb") {
                Some(id) => {
                    let _ = SB_ID.set(id);
                    Some(id)
                }
                None => None,
            },
        };
        let existing = match sb_id {
            Some(id) => self.get_field_by_id(handle, id),
            None => self.get_field_syn(handle, crate::heap::Syn::Sb),
        };
        let arr = match existing {
            Value::Array(a) => a,
            _ => {
                let a = self.alloc_array("char", 0);
                self.put_field_syn(handle, crate::heap::Syn::Sb, Value::Array(a));
                a
            }
        };
        let mut added = 0usize;
        if let Some(a) = self.arrays.write().unwrap_or_else(|e| e.into_inner()).get_mut(arr as usize) {
            if let ArrayData::Int(v) = &mut a.data {
                v.reserve(text.len());
                for c in text.chars() {
                    v.push(c as i32);
                    added += 1;
                }
            }
        }
        // char buffer is i32-backed (elem_bytes 4) — match the sweep, not SLOT_BYTES.
        // Gated → default build keeps its exact prior `SLOT_BYTES` estimate.
        #[cfg(feature = "next-gen-gc")]
        {
            self.bytes.fetch_add(added * 4, std::sync::atomic::Ordering::Relaxed);
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            self.bytes.fetch_add(added * SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
        }
        self.sync_array_desc(arr);
    }

    /// Replace an object's `$sb` buffer with exactly `text` (one resync).
    pub fn sb_set(&mut self, handle: u32, text: &str) {
        let a = self.alloc_array("char", 0);
        let mut added = 0usize;
        if let Some(arr) = self.arrays.write().unwrap_or_else(|e| e.into_inner()).get_mut(a as usize) {
            if let ArrayData::Int(v) = &mut arr.data {
                v.reserve(text.len());
                for c in text.chars() {
                    v.push(c as i32);
                    added += 1;
                }
            }
        }
        // char buffer is i32-backed (elem_bytes 4) — match the sweep, not SLOT_BYTES.
        // Gated → default build keeps its exact prior `SLOT_BYTES` estimate.
        #[cfg(feature = "next-gen-gc")]
        {
            self.bytes.fetch_add(added * 4, std::sync::atomic::Ordering::Relaxed);
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            self.bytes.fetch_add(added * SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
        }
        self.sync_array_desc(a);
        self.put_field_syn(handle, crate::heap::Syn::Sb, Value::Array(a));
    }

    // --- map backings -------------------------------------------------------
}

/// Widen `bytes` into `d[off..]`, unsigned, returning how many landed.
///
/// ⭐**The clamp is computed ONCE and the copy then runs over SLICES.** The previous form indexed
/// `d[off + k]` inside the loop, so every char carried a bounds check LLVM could not fold away —
/// `off` and `k` are two separate values and the proof that their sum is in range lives in an
/// arithmetic `min` several statements earlier. Zipping two exactly-sized slices states the same
/// clamp in the type system: the loop has no branch left in it and lowers to a widening load/store
/// (`pmovzxbd`) instead of a per-element test. Same answer, same clamp, same unsigned widening.
#[inline]
fn widen_run(d: &mut [i32], off: usize, bytes: &[u8]) -> usize {
    if off >= d.len() {
        return 0;
    }
    let n = bytes.len().min(d.len() - off);
    for (o, &b) in d[off..off + n].iter_mut().zip(&bytes[..n]) {
        *o = b as i32;
    }
    n
}

/// `widen_run` with the source run named by position/length instead of already copied out.
#[inline]
fn widen_slice(s: &[u8], spos: usize, d: &mut [i32], doff: usize, want: usize) -> usize {
    // `array_read_bytes` answers an empty slice — not `None` — once the position is at or past the
    // end, and the caller turns 0 into EOF. Same answer here.
    if spos >= s.len() {
        return 0;
    }
    let avail = (s.len() - spos).min(want);
    widen_run(d, doff, &s[spos..spos + avail])
}
