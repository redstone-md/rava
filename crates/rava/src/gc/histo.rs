//! `RAVA_GC_HISTO`: after each full collection's mark, a histogram of the live heap (cells,
//! object bytes, the classes holding most), and what the same objects would take with 8-byte
//! size classes and with a compact layout (a 12-byte header, 4-byte references), for sizing
//! layout work.

use super::space::{Space, PAGE_SIZE};
use crate::heap::{obj, Oop, ARRAY_BASE_OFFSET};
use crate::klass::KlassKind;
use std::collections::HashMap;

#[derive(Default)]
struct Totals {
    objects: usize,
    cells: usize,
    bytes: usize,
    step8: usize,
    compact: usize,
}

pub fn report(space: &Space) {
    let mut all = Totals::default();
    let mut by_class: HashMap<&'static str, (usize, usize)> = HashMap::new();
    for (index, meta) in space.pages.iter().enumerate() {
        let Some(meta) = meta else { continue };
        let page = space.page_addr(index);
        let cell = meta.cell_size as usize;
        for c in 0..PAGE_SIZE / cell {
            if meta.marks[c / 64] & (1 << (c % 64)) == 0 {
                continue;
            }
            let o = (page + c * cell) as Oop;
            // SAFETY: a marked cell holds a live object.
            let k = unsafe { obj::klass(o) };
            let (bytes, compact) = match k.kind {
                KlassKind::Instance => {
                    let size = k.instance_size() as usize;
                    (size, size - 16 + 12 - 4 * k.ref_offsets.len())
                }
                _ => {
                    // SAFETY: an array.
                    let len = unsafe { obj::array_length(o) } as usize;
                    let elem = k.elem_size() as usize;
                    let celem = if matches!(k.kind, KlassKind::ObjArray { .. }) { 4 } else { elem };
                    (ARRAY_BASE_OFFSET as usize + elem * len, 16 + celem * len)
                }
            };
            all.objects += 1;
            all.cells += cell;
            all.bytes += bytes;
            all.step8 += if bytes <= 256 { bytes.next_multiple_of(8) } else { cell };
            all.compact += if compact <= 256 { compact.next_multiple_of(8) } else { compact.next_multiple_of(64) };
            let e = by_class.entry(k.name.as_str()).or_default();
            e.0 += 1;
            e.1 += cell;
        }
    }
    let mb = |b: usize| b >> 20;
    eprintln!(
        "[gc-histo] {} objects: cells {} MiB, objects {} MiB, with 8-byte classes {} MiB, compact layout {} MiB; large {} MiB",
        all.objects,
        mb(all.cells),
        mb(all.bytes),
        mb(all.step8),
        mb(all.compact),
        mb(space.large.values().filter(|l| l.marked).map(|l| l.size).sum())
    );
    let mut top: Vec<_> = by_class.into_iter().collect();
    top.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
    for (name, (n, b)) in top.iter().take(25) {
        eprintln!("[gc-histo]   {:>7} KiB {:>9} {}", b >> 10, n, name);
    }
}
