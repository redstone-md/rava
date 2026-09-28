//! The cell census that runs at every collection when RAVA_CELL_CENSUS is set.
//!
//! Read-only: it walks the arena and reports what the cells hold, so a representation
//! question can be answered from a real boot instead of an argument.

use crate::vm::gc_census_report::cell_census_report;
use crate::*;

impl Vm {
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn gc_cell_census(&mut self) {
        #[cfg(feature = "next-gen-gc")]
        if cell_census_enabled() {
            use std::io::Write as _;
            // TARGET histogram: the holder says WHERE the blocking cells are, the
            // target says WHICH class must migrate to clear them.
            let mut targets: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            // DISTINCT handles behind each target's cell count. A cell count alone cannot say
            // how much work clearing it is, and for the enum targets it hid the whole question:
            // RAVA_ARENA_ENUM demonstrably arena-births 936 enum constants (birth census, zero
            // late escapes) yet PushReaction still shows 30,837 handle cells to the digit. Seven
            // distinct handles would mean the constants themselves are still heap-side; tens of
            // thousands would mean these are not constants at all and the enum lever is aimed
            // at the wrong population.
            let mut target_ids: std::collections::HashMap<String, std::collections::HashSet<u32>> =
                std::collections::HashMap::new();
            // CONTENT of the two non-handle buckets. Both turned out to be mixtures:
            // the array bucket holds internal growable storage ($elems/$sb/$buf — not
            // Java arrays at all, so not part of an array migration), and the box
            // bucket mixes strings, boxed primitives and sentinels, each needing a
            // different mechanism. Counting them apart is what makes the buckets
            // rankable by WORK rather than by cell share.
            let mut arrays: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            let mut boxes: std::collections::HashMap<&'static str, u64> = std::collections::HashMap::new();
            // BIT BUDGET, MEASURED. A narrower cell (e.g. 4-byte reference cells) has to
            // fit each tagged arm's PAYLOAD in `width - 2` bits. `(handle << 2) | 1` with a
            // u32 handle already needs 34, so the question is not the type but the largest
            // value that is actually ever STORED in a cell. Track the max per arm — that,
            // not the count, is what decides whether a narrowing is representable.
            let (mut max_handle, mut max_array, mut max_box) = (0u32, 0u32, 0usize);
            // Row 79's split: array cells held in rava's SYNTHETIC collection storage vs in a
            // field the guest declares. Sizes the real remaining flip work for the first time.
            let (mut arr_synth, mut arr_guest) = (0u64, 0u64);
            // Same split for the HANDLE bucket — the one that carries the whole remaining
            // residue. Synthetic holders are unreachable by a bare deref and so are not
            // blockers at all; only guest-declared holders are.
            let (mut h_synth, mut h_guest) = (0u64, 0u64);
            let mut h_by_holder: std::collections::HashMap<(u32, usize, String), u64> =
                std::collections::HashMap::new();
            // (holder class id, cell slot) -> array cells held there. Resolved to declared
            // descriptors after the walk (row 80): a field declared `[...` can never hold a
            // plain object reference, so the bare-deref flip needs no tag test on it and its
            // cells are not blockers at all — which decides whether arrays must migrate.
            let mut arr_by_holder: std::collections::HashMap<(u32, usize), u64> =
                std::collections::HashMap::new();
            let rows = {
                let heap = &self.heap;
                let mut on_handle = |h: u32, cid: u32, slot: usize| {
                    max_handle = max_handle.max(h);
                    // `class_of` returning None became the DOMINANT target the moment the
                    // synthetic migrations landed (14,576 cells, 81% of the residue), and a
                    // bucket called "?" cannot be acted on. Split it by whether the handle is
                    // even in range: an in-range handle with no class is a SWEPT slot (a cell
                    // still naming a dead object), an out-of-range one is not a handle at all
                    // and means a cell is being misread as 0b01.
                    let cls = match heap.class_of(h) {
                        Some(c) => c,
                        None if heap.is_ref_alive(h) => "?alive-but-unnamed",
                        None => "?dead-or-out-of-range",
                    };
                    *targets.entry(cls.to_string()).or_insert(0) += 1;
                    target_ids.entry(cls.to_string()).or_default().insert(h);
                    // A SYNTHETIC holder is unreachable by any bare-deref: no bytecode names
                    // a `$…` field, so the JIT emits no getfield for it, and the natives that
                    // read it use the tag-aware accessors. Such a cell can keep its tag for
                    // free — exactly the argument that took 211,141 array cells out of the
                    // blocker set — so counting it as a blocker over-states the work left.
                    let synth = crate::nextgen::class_layout(cid)
                        .and_then(|(_, l)| l.get(slot).map(|&(fid, _)| fid))
                        .and_then(|fid| heap.field_name(fid))
                        .map_or(false, |n: String| n.starts_with('$') || n.contains("#$"));
                    if synth {
                        h_synth += 1;
                    } else {
                        h_guest += 1;
                    }
                    // Keyed by (holder, target) so the post-walk pass can name WHICH classes
                    // the genuinely-blocking cells point at — the whole remaining question.
                    *h_by_holder.entry((cid, slot, cls.to_string())).or_insert(0u64) += 1;
                };
                let mut on_array = |h: u32, cid: u32, slot: usize| {
                    max_array = max_array.max(h);
                    let el = heap
                        .array_element(h)
                        .as_deref()
                        .unwrap_or("?")
                        .to_string();
                    *arrays.entry(el).or_insert(0) += 1;
                    // SYNTHETIC vs GUEST holder. A `$`-prefixed field name is rava's own
                    // storage for a native-backed collection (`$elems`/`$sb`/`$buf`); anything
                    // else is a field the guest class actually declares. That single character
                    // is what separates "these cells vanish when the collections migrate" from
                    // "these need a real array representation", and rows 72/75 were scoped
                    // without it.
                    let synth = crate::nextgen::class_layout(cid)
                        .and_then(|(_, l)| l.get(slot).map(|&(fid, _)| fid))
                        .and_then(|fid| heap.field_name(fid))
                        .map_or(false, |n: String| n.starts_with('$') || n.contains("#$"));
                    if synth {
                        arr_synth += 1;
                    } else {
                        arr_guest += 1;
                    }
                    // Row 80: tally the HOLDER, resolve its declared descriptor afterwards.
                    // The descriptor needs the class loader, and `heap` is already borrowed by
                    // this closure — collecting (class, slot) here and resolving after the walk
                    // keeps both borrows honest and costs one hash per array cell.
                    *arr_by_holder.entry((cid, slot)).or_insert(0u64) += 1;
                };
                let mut on_box = |i: usize| {
                    max_box = max_box.max(i);
                    let k = match crate::nextgen::arena_box_get(i) {
                        Value::Str(_) => "Str",
                        Value::Int(_) => "Int",
                        Value::Long(_) => "Long",
                        Value::Float(_) => "Float",
                        Value::Double(_) => "Double",
                        Value::Null => "Null/dead",
                        _ => "sentinel/other",
                    };
                    *boxes.entry(k).or_insert(0) += 1;
                };
                crate::nextgen::cell_census(
                    crate::nextgen::registry_len(),
                    &mut on_handle,
                    &mut on_array,
                    &mut on_box,
                )
            };
            let mut tot = [0u64; 4];
            for (_, c) in &rows {
                for i in 0..4 {
                    tot[i] += c[i];
                }
            }
            let blocking = tot[1] + tot[2] + tot[3];
            // HEADER CEILING: what a header shrink is actually worth on THIS live set.
            // Reported on its own PEAK-LIVE-OBJECTS guard, NOT the flip-blocking one:
            // the blocking count is already 0 on this workload (100% bare-ready), so a
            // print nested under `blocking > peak` never fires.
            {
                let (hobjs, hbytes, hcells, hrefs, hnarrow) =
                    crate::nextgen::header_census(crate::nextgen::registry_len());
                let mut hpeak = HDR_CENSUS_PEAK.lock().unwrap_or_else(|e| e.into_inner());
                if hobjs > *hpeak {
                    *hpeak = hobjs;
                    let hdr_bytes = hobjs * crate::nextgen::HDR as u64;
                    eprintln!(
                        "[HDR-CENSUS] live objs {hobjs} cells {hcells} bytes {hbytes} ({:.1} MB) | \
                         header {hdr_bytes}B = {:.1}% of live | 24->16 saves {}B ({:.2} MB, {:.1}%) | \
                         24->8 saves {}B ({:.2} MB, {:.1}%) | mean cells/obj {:.2}",
                        hbytes as f64 / (1024.0 * 1024.0),
                        hdr_bytes as f64 * 100.0 / hbytes as f64,
                        hobjs * 8,
                        (hobjs * 8) as f64 / (1024.0 * 1024.0),
                        (hobjs * 8) as f64 * 100.0 / hbytes as f64,
                        hobjs * 16,
                        (hobjs * 16) as f64 / (1024.0 * 1024.0),
                        (hobjs * 16) as f64 * 100.0 / hbytes as f64,
                        hcells as f64 / hobjs as f64,
                    );
                    // CELL CEILING — the other half of the object. `naive` is the number a
                    // 4-byte-ref-cell proposal is usually argued from; `real` is what 8-byte
                    // object alignment actually leaves on the table (8 * floor(nref/2) per
                    // object — one ref cell saves NOTHING). The gap between them IS the
                    // padding, and it is the number to compare against the header shrink's.
                    let naive = hrefs * 4;
                    eprintln!(
                        "[HDR-CENSUS] ref cells {hrefs} ({:.1}% of cells) | 8->4B ref cells: \
                         naive {naive}B ({:.2} MB) vs REAL after 8B object alignment {hnarrow}B \
                         ({:.2} MB, {:.1}% of live) = {:.0}% of naive | header 24->16 was {:.2} MB \
                         => ratio {:.2}x | box-table high-water {}",
                        hrefs as f64 * 100.0 / hcells as f64,
                        naive as f64 / (1024.0 * 1024.0),
                        hnarrow as f64 / (1024.0 * 1024.0),
                        hnarrow as f64 * 100.0 / hbytes as f64,
                        if naive > 0 { hnarrow as f64 * 100.0 / naive as f64 } else { 0.0 },
                        (hobjs * 8) as f64 / (1024.0 * 1024.0),
                        hnarrow as f64 / (hobjs * 8) as f64,
                        crate::nextgen::arena_box_len(),
                    );
                    // BIT BUDGET per tagged arm. A 4-byte cell leaves 30 payload bits
                    // (`payload << 2 | tag`), so each arm must stay under 2^30.
                    let bits = |v: u64| if v == 0 { 0 } else { 64 - v.leading_zeros() };
                    eprintln!(
                        "[HDR-CENSUS] cell payload high-water: handle {max_handle} ({} bits) | \
                         array {max_array} ({} bits) | box {max_box} ({} bits) | \
                         a 4B cell allows 30 bits (2^30 = {})",
                        bits(max_handle as u64),
                        bits(max_array as u64),
                        bits(max_box as u64),
                        1u64 << 30,
                    );
                    // THE ARM THAT ACTUALLY BINDS. The untagged 0b00 arm is not limited by a
                    // payload magnitude but by ARENA VA SPAN: 30 payload bits at 8-byte object
                    // alignment reach only 8 GiB above ARENA_FLOOR, while `map_low` budgets
                    // NEXT_LOW against a 2^40 ceiling and burns span on every reserve (incl.
                    // failed try_map_at retries). Report how far the cursor has actually walked.
                    let span = crate::nextgen::arena_high_water() - crate::nextgen::ARENA_FLOOR;
                    eprintln!(
                        "[HDR-CENSUS] arena VA span used {span}B ({:.2} GiB) of the {} GiB a 30-bit \
                         0b00 cell could address ({:.1}% consumed); ceiling today is 2^40",
                        span as f64 / (1024.0 * 1024.0 * 1024.0),
                        8,
                        span as f64 * 100.0 / (8.0 * 1024.0 * 1024.0 * 1024.0),
                    );
                }
            }
            cell_census_report(self, &targets, &target_ids, &arrays, &boxes, &h_by_holder, &arr_by_holder, &rows,
                arr_synth, arr_guest, h_synth, h_guest, max_handle, max_array, max_box, tot, blocking);
        }
    }
}
