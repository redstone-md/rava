//! The flat shadow: constructing it, checking it at return, and dumping it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::shadow::*;

/// Called at a record decode's jit_call return. If the top record frame is an all-primitive flatten
/// target, extract the DFU result object and compare each field's native flat read to the DFU field.
pub fn flat_at_return(vm: &mut Vm, result_bits: i64) {
    let verify = flat_enabled();
    if !verify && !flat_arm_enabled() {
        return;
    }
    let c = FLAT_CALLED.fetch_add(1, Relaxed) + 1;
    if c == 5_000 || c == 50_000 || c % 500_000 == 0 {
        if verify {
            flat_dump(); // diag milestone keyed on CALLS so we see progress even at 0 records
        }
        arm_dump();
    }
    let Some((input, fields)) = crate::recordast::top_flat_target() else { return };
    let t = FLAT_TARGET.fetch_add(1, Relaxed) + 1;
    let res_v = crate::jit::unbox_ref(vm, result_bits, "");
    if verify && t <= 12 {
        let fnames: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        eprintln!(
            "[SHADOW-FLAT-DIAG] bits={result_bits:#x} res_v={res_v:?} res_class={:?} input_class={:?} fields={fnames:?}",
            val_class(vm, &res_v),
            val_class(vm, &input)
        );
    }
    let Some(obj) = extract_result_from_data_result(vm, &res_v) else {
        FLAT_EXTRACT_FAIL.fetch_add(1, Relaxed);
        return;
    };
    let rn = FLAT_RECORDS.fetch_add(1, Relaxed) + 1;
    if verify && (rn == 500 || rn == 5_000 || rn == 50_000 || rn % 200_000 == 0) {
        flat_dump();
    }
    for (name, _kind) in &fields {
        if !verify {
            break; // arm-only run: skip the 24.3 per-field read-compare, go straight to construct-shadow
        }
        // DFU-decoded object's field is the ground truth. The codec key is snake_case; the Java field
        // is usually camelCase — try both. A miss = the codec key isn't a stored field (dispatch "type"
        // discriminator, or a synthesised/renamed component) → not a flat target.
        let Some(dfu) = dfu_field(vm, &obj, name) else {
            FLAT_DFU_FAIL.fetch_add(1, Relaxed);
            let u = FLAT_UNMAPPED.fetch_add(1, Relaxed) + 1;
            if u <= 24 {
                eprintln!(
                    "[SHADOW-FLAT-UNMAPPED] field={name} obj_class={:?} reason=dfu-miss",
                    val_class(vm, &obj)
                );
            }
            continue;
        };
        // A flat primitive stores a scalar. If DFU decoded this key into an object (registry entry,
        // ResourceLocation, TagKey, nested record) it is NOT flattenable — count separately, don't mismatch.
        if !is_scalar(&dfu) {
            let np = FLAT_NONPRIM.fetch_add(1, Relaxed) + 1;
            if np <= 24 {
                eprintln!(
                    "[SHADOW-FLAT-NONPRIM] field={name} obj_class={:?} dfu={dfu:?} (string→object decode, not flat)",
                    val_class(vm, &obj)
                );
            }
            continue;
        }
        // Genuine scalar field: read the raw JSON value and compare type-directed against the DFU scalar.
        let Some(nv) = get_field_from_object(vm, &input, name) else {
            FLAT_NATIVE_FAIL.fetch_add(1, Relaxed);
            continue;
        };
        match native_matches_scalar(vm, &nv, &dfu) {
            Some(true) => {
                FLAT_MATCH.fetch_add(1, Relaxed);
            }
            Some(false) => {
                let c = FLAT_MISMATCH.fetch_add(1, Relaxed) + 1;
                if c <= 40 {
                    eprintln!(
                        "[SHADOW-FLAT-MISMATCH] field={name} native={:?} dfu={dfu:?}",
                        native_scalar(vm, &nv)
                    );
                }
            }
            None => {
                // Native shape we couldn't reduce to a scalar — count as native-fail, not mismatch.
                FLAT_NATIVE_FAIL.fetch_add(1, Relaxed);
            }
        }
    }
    // 24.4 CONSTRUCT-SHADOW: if arming is enabled, materialize a full guest object via decode_flat
    // (native scalar reads → the record's REAL ctor) and compare its ENTIRE field-set to the DFU
    // object. Proves the WRITE/construct chain (alloc + ctor-arg-order + store) bit-exact on the real
    // boot stream, WITHOUT returning it — the boot still uses the combinator result, so a construct
    // bug can't corrupt boot state (the shadow-before-arm shield). Default-ON only after this is clean.
    if flat_arm_enabled() {
        flat_construct_shadow(vm, &input, &fields, &obj);
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// 24.4 CONSTRUCT-SHADOW / decode_flat (RAVA_JIT_FLAT_ARM). The materialized flat decoder: for a record
// whose EVERY component is a native-readable scalar, read each field straight from `input`, typed to the
// record's canonical-ctor parameter, and construct the object via its REAL constructor (new_instance) —
// inheriting any compact-ctor normalization/validation, so the result is bit-identical to the combinator
// decode (raw offset-stores would skip that logic → a determinism gamble). This kills the 12-floor
// FieldDecoder→Pair/Optional/DataResult tower: N field reads + 1 ctor, no per-field monad allocs.
/// Build the object natively and compare its whole field-set to the DFU result. Shadow-only (discarded).
pub(crate) fn flat_construct_shadow(vm: &mut Vm, input: &Value, fields: &[(String, u8)], obj: &Value) {
    let Some(class) = val_class(vm, obj) else { return };
    let Some(ifields) = instance_fields(vm, &class) else { return };
    // Armable iff the record's component count matches the codec's field count AND every component is a
    // native-readable scalar/String. Any object component (registry entry, nested record) → not armable.
    // 24.5 instrumentation: bucket the blockers so the highest-yield extension (sub-record recursion vs
    // list loop vs registry callout) is chosen from data, not guesswork.
    if ifields.is_empty() {
        ARM_NOTARMABLE.fetch_add(1, Relaxed);
        return;
    }
    if ifields.len() != fields.len() {
        ARM_NOTARMABLE.fetch_add(1, Relaxed);
        NOTARM_COUNT.fetch_add(1, Relaxed);
        note_count_mismatch(&class, ifields.len(), fields.len());
        return;
    }
    if !ifields.iter().all(|(_, d)| is_flat_descriptor(d)) {
        ARM_NOTARMABLE.fetch_add(1, Relaxed);
        // Classify + sample the FIRST non-flat component: [=list/array, L…=object (record vs registry).
        if let Some((fname, d)) = ifields.iter().find(|(_, d)| !is_flat_descriptor(d)) {
            if d.starts_with('[') {
                NOTARM_ARRAY.fetch_add(1, Relaxed);
            } else {
                NOTARM_OBJECT.fetch_add(1, Relaxed);
            }
            note_blocker(d, fname);
        }
        return;
    }
    // Codec field order == ctor param order == declaration order (RecordCodecBuilder applies the group in
    // order). Read each component from `input` by its codec key, typed to the ctor param descriptor.
    let mut arg_desc = String::new();
    let mut args: Vec<Value> = Vec::with_capacity(fields.len());
    for (i, (codec_key, _)) in fields.iter().enumerate() {
        let desc = &ifields[i].1;
        let Some(v) = native_typed(vm, input, codec_key, desc) else {
            ARM_BUILD_FAIL.fetch_add(1, Relaxed);
            return;
        };
        arg_desc.push_str(desc);
        args.push(v);
    }
    // Construct via the real canonical ctor. new_instance heap-allocs + runs <init> (the same re-entrant
    // vm.invoke the combinator fallback path uses) → correct ctor semantics, bit-exact fields.
    let shadow = match crate::reflect::new_instance(vm, &class, &arg_desc, &args) {
        Ok(v) => v,
        Err(_) => {
            ARM_BUILD_FAIL.fetch_add(1, Relaxed);
            return;
        }
    };
    let rn = ARM_RECORDS.fetch_add(1, Relaxed) + 1;
    if rn == 500 || rn == 5_000 || rn == 50_000 || rn % 200_000 == 0 {
        arm_dump();
    }
    // Compare EVERY declared field (not just the codec ones): shadow vs DFU. Catches ctor-derived /
    // reordered / defaulted fields a field-by-field read would miss.
    for (fname, fdesc) in &ifields {
        let s = read_guest_field(vm, &shadow, fname);
        let d = read_guest_field(vm, obj, fname);
        let eq = match (&s, &d) {
            (Some(a), Some(b)) => scalar_field_eq(vm, a, b, fdesc),
            (None, None) => true,
            _ => false,
        };
        if eq {
            ARM_FMATCH.fetch_add(1, Relaxed);
        } else {
            let c = ARM_FMISMATCH.fetch_add(1, Relaxed) + 1;
            if c <= 40 {
                eprintln!(
                    "[FLAT-ARM-MISMATCH] class={class} field={fname}:{fdesc} shadow={s:?} dfu={d:?}"
                );
            }
        }
    }
}

pub fn arm_dump() {
    if !flat_arm_enabled() {
        return;
    }
    let (r, m, mm) = (
        ARM_RECORDS.load(Relaxed),
        ARM_FMATCH.load(Relaxed),
        ARM_FMISMATCH.load(Relaxed),
    );
    let cmp = m + mm;
    let pct = if cmp > 0 { m * 10000 / cmp } else { 0 };
    eprintln!(
        "[FLAT-ARM] constructed={r} not-armable={} build-fail={}  | ALL-field compares: match={m} mismatch={mm} ({}.{:02}% match)",
        ARM_NOTARMABLE.load(Relaxed),
        ARM_BUILD_FAIL.load(Relaxed),
        pct / 100,
        pct % 100
    );
    // 24.5 coverage-lock breakdown: where the not-armable records are blocked.
    eprintln!(
        "[FLAT-ARM] not-armable by reason: count-mismatch={} object-component={} array-component={}",
        NOTARM_COUNT.load(Relaxed),
        NOTARM_OBJECT.load(Relaxed),
        NOTARM_ARRAY.load(Relaxed)
    );
    let g = BLOCKERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = g.as_ref() {
        let mut rows: Vec<(&String, &u64)> = map.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!("[FLAT-ARM] top blocking components (field:descriptor → count) — the 24.5 flatten targets:");
        for (k, n) in rows.iter().take(20) {
            eprintln!("[FLAT-ARM]   {:>7}  {}", n, k);
        }
    }
    drop(g);
    let cg = COUNTMIS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = cg.as_ref() {
        let mut rows: Vec<(&String, &u64)> = map.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!("[FLAT-ARM] top count-mismatch classes (declared instance fields vs observed codec fields):");
        for (k, n) in rows.iter().take(15) {
            eprintln!("[FLAT-ARM]   {:>7}  {}", n, k);
        }
    }
    if mm == 0 && r > 0 {
        eprintln!("[FLAT-ARM] ⇒ decode_flat CONSTRUCT == DFU decode on ALL fields of every armable record — the alloc+ctor+store chain is proven.");
    }
}

pub fn flat_dump() {
    if !flat_enabled() {
        return;
    }
    let (r, m, mm, um) = (
        FLAT_RECORDS.load(Relaxed),
        FLAT_MATCH.load(Relaxed),
        FLAT_MISMATCH.load(Relaxed),
        FLAT_UNMAPPED.load(Relaxed),
    );
    let cmp = m + mm;
    let pct = if cmp > 0 { m * 10000 / cmp } else { 0 };
    eprintln!(
        "[SHADOW-FLAT] diag: at_return_called={} target={} extract_fail={} native_fail={} dfu_namemiss={} nonprim(str→obj)={}",
        FLAT_CALLED.load(Relaxed),
        FLAT_TARGET.load(Relaxed),
        FLAT_EXTRACT_FAIL.load(Relaxed),
        FLAT_NATIVE_FAIL.load(Relaxed),
        FLAT_DFU_FAIL.load(Relaxed),
        FLAT_NONPRIM.load(Relaxed)
    );
    eprintln!(
        "[SHADOW-FLAT] records={r}  | SCALAR field compares: match={m} mismatch={mm} ({}.{:02}% match)  name-miss={um}",
        pct / 100,
        pct % 100
    );
    if mm == 0 && m > 0 {
        eprintln!("[SHADOW-FLAT] ⇒ native flat read == DFU decode on ALL compared primitive fields — the flat READ chain is proven (24.4 = materialize alloc+fill).");
    }
}
