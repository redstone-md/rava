//! Snapshotting the arena and restoring it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Everything the arena needs to reconstruct BYTE-IDENTICALLY at the SAME addresses.
/// Every region maps at a fixed base (`try_map_at` never relocates), so the raw arena
/// pointers inside the serialized heap/statics/frames stay valid after a restore.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ArenaSnapshot {
    pub(crate) young: (u64, usize, usize, Vec<u8>), // (base, cap, top, bytes[..top])
    pub(crate) old: (u64, usize, usize, Vec<u8>),
    pub(crate) perm: (u64, usize, usize, Vec<u8>),
    pub(crate) perm_map: std::collections::HashMap<String, usize>,
    /// The class registry in id order (name, layout, uniform-ref marker, optional array component)
    /// — ids are positions, so re-registering in order reproduces every id the object headers
    /// reference, including exact reference-array metadata.
    pub(crate) classes: Vec<(String, Vec<(u32, u8)>, bool, Option<String>)>,
    pub(crate) boxes: Vec<crate::value::Value>,
    pub(crate) gc_epoch: u64,
}

/// Capture the current thread's arena (run at a checkpoint frame boundary, after a
/// collection — so `young` is small and `old` is the compacted live set).
pub fn snapshot_arena() -> Option<ArenaSnapshot> {
    crate::nextgen::tlab::with_tlab(|slot| {
        let h = slot.as_ref()?;
        let pg = perm_pool().lock().unwrap();
        let reg = registry().lock().unwrap();
        Some(ArenaSnapshot {
            young: (
                h.young.ptr as u64,
                h.young.len,
                h.young.top,
                unsafe { std::slice::from_raw_parts(h.young.ptr, h.young.top) }.to_vec(),
            ),
            old: (
                h.old.ptr as u64,
                h.old.len,
                h.old.top,
                unsafe { std::slice::from_raw_parts(h.old.ptr, h.old.top) }.to_vec(),
            ),
            perm: (
                pg.0.base as u64,
                pg.0.cap,
                pg.0.top,
                unsafe { std::slice::from_raw_parts(pg.0.base, pg.0.top) }.to_vec(),
            ),
            perm_map: pg.1.clone(),
            classes: reg
                .iter()
                .map(|c| {
                    (
                        c.name.to_string(),
                        c.layout.to_vec(),
                        c.all_refs,
                        c.array_component.as_ref().map(ToString::to_string),
                    )
                })
                .collect(),
            boxes: arena_box().lock().unwrap().clone(),
            gc_epoch: GC_EPOCH.load(Ordering::Relaxed),
        })
    })
}

/// Rebuild the arena from a snapshot at the SAME addresses. Must run on the main
/// thread BEFORE any arena allocation (a fresh TLAB would otherwise claim the fixed
/// bases with an empty nursery).
pub fn restore_arena(s: &ArenaSnapshot) -> Result<(), String> {
    // 1. Classes (ids must match the ids baked into the restored object headers).
    //    A fresh process may have pre-registered a few classes already — they must be
    //    a prefix-match of the snapshot's registry, else the header ids lie.
    {
        let mut r = registry().lock().unwrap();
        for (i, (name, layout, all_refs, component)) in s.classes.iter().enumerate() {
            if let Some(existing) = r.get(i) {
                let existing_component = existing.array_component.as_deref();
                if &*existing.name != name.as_str()
                    || existing.all_refs != *all_refs
                    || existing_component != component.as_deref()
                {
                    return Err(format!(
                        "arena restore: registry mismatch at id {i}: {} (all_refs={}, component={existing_component:?}) vs {name} (all_refs={all_refs}, component={component:?})",
                        existing.name, existing.all_refs,
                    ));
                }
                continue;
            }
            if component.is_some() && !*all_refs {
                return Err(format!(
                    "arena restore: class {name} carries a component descriptor without all_refs"
                ));
            }
            let l: Arc<[(u32, u8)]> = layout.clone().into();
            let words = (l.len() + 63) / 64;
            let mut ref_mask = vec![0u64; words.max(1)];
            for (j, &(_, kind)) in l.iter().enumerate() {
                if kind >= 4 {
                    ref_mask[j / 64] |= 1u64 << (j % 64);
                }
            }
            r.push(ArenaClass {
                name: Arc::from(name.as_str()),
                layout: l,
                ref_mask: ref_mask.into(),
                all_refs: *all_refs,
                array_component: component
                    .as_deref()
                    .map(|s| Arc::from(s)),
            });
        }
        if r.len() > s.classes.len() {
            return Err(format!(
                "arena restore: live registry ({}) larger than snapshot ({}) — restore must run before arena use",
                r.len(),
                s.classes.len()
            ));
        }
    }
    // 2. Regions at their recorded bases.
    crate::nextgen::tlab::with_tlab(|slot| -> Result<(), String> {
        if slot.is_some() {
            return Err("arena restore: TLAB already initialised".into());
        }
        let (yb, yc, yt, ybytes) = &s.young;
        let (ob, oc, ot, obytes) = &s.old;
        let young = Region::restored(*yb, *yc, *yt, ybytes, false)?;
        let old = Region::restored(*ob, *oc, *ot, obytes, true)?;
        *slot = Some(GenHeap { young, old, shapes: Vec::new(), minor_gcs: 0, copied: 0 });
        if let Some(h) = slot.as_ref() {
            refresh_cursor(h);
        }
        Ok(())
    })?;
    // 3. Permanent interned-string pool. `perm_pool()` may already have mapped its
    //    default region — if the base differs from the snapshot's, remap at the
    //    recorded base (the old mapping is simply left unused).
    {
        let (pb, pc, pt, pbytes) = &s.perm;
        let m = perm_pool();
        let mut g = m.lock().unwrap();
        if g.0.base as u64 != *pb {
            let base = try_map_at(*pb, *pc, true)
                .ok_or_else(|| format!("arena restore: perm base {pb:#x} unavailable"))?;
            g.0 = PermPool { base, top: 0, cap: *pc };
        }
        unsafe { std::ptr::copy_nonoverlapping(pbytes.as_ptr(), g.0.base, (*pt).min(pbytes.len())) };
        g.0.top = *pt;
        g.1 = s.perm_map.clone();
    }
    // 4. Boxed-value side table + epoch.
    *arena_box().lock().unwrap() = s.boxes.clone();
    GC_EPOCH.store(s.gc_epoch, Ordering::Relaxed);
    Ok(())
}
