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
    /// 12-byte header, 8-byte references; 8-byte header, 4-byte references.
    header12: usize,
    header8: usize,
}

/// Bytes of an instance's fields (with references of `reference` bytes), the inherited included.
fn field_bytes(k: &'static crate::klass::Klass, reference: usize) -> usize {
    let own: usize = k.fields.iter().filter(|f| !f.is_static()).map(|f| if f.ty == crate::types::BasicType::Reference { reference } else { f.ty.size() as usize }).sum();
    own + k.super_klass.map_or(0, |s| field_bytes(s, reference))
}

pub fn report(space: &Space) {
    let mut all = Totals::default();
    let mut by_class: HashMap<&'static str, (usize, usize)> = HashMap::new();
    for (index, meta) in space.pages.iter().enumerate() {
        let Some(meta) = meta else { continue };
        let page = space.page_addr(index);
        let mut objects = Vec::new();
        if meta.is_mixed() {
            super::mixed::each(&meta.marks, page, page, page + PAGE_SIZE, |at, size| objects.push((at, size)));
        } else {
            let cell = meta.cell_size as usize;
            objects.extend((0..PAGE_SIZE / cell).filter(|c| meta.marks[c / 64] & (1 << (c % 64)) != 0).map(|c| (page + c * cell, cell)));
        }
        for (at, cell) in objects {
            let o = at as Oop;
            // SAFETY: a marked cell holds a live object.
            let k = unsafe { obj::klass(o) };
            let (bytes, compact, h12, h8) = match k.kind {
                KlassKind::Instance => {
                    let size = k.instance_size() as usize;
                    (size, 12 + field_bytes(k, 4), 12 + field_bytes(k, 8), 8 + field_bytes(k, 4))
                }
                _ => {
                    // SAFETY: an array.
                    let len = unsafe { obj::array_length(o) } as usize;
                    let elem = k.elem_size() as usize;
                    let celem = if matches!(k.kind, KlassKind::ObjArray { .. }) { 4 } else { elem };
                    (ARRAY_BASE_OFFSET as usize + elem * len, 16 + celem * len, 16 + elem * len, 12 + celem * len)
                }
            };
            all.objects += 1;
            all.cells += cell;
            all.bytes += bytes;
            all.step8 += if bytes <= 256 { bytes.next_multiple_of(8) } else { cell };
            let rounded = |b: usize| if b <= 256 { b.next_multiple_of(8) } else { b.next_multiple_of(64) };
            all.compact += rounded(compact);
            all.header12 += rounded(h12);
            all.header8 += rounded(h8);
            let e = by_class.entry(k.name.as_str()).or_default();
            e.0 += 1;
            e.1 += cell;
        }
    }
    let mb = |b: usize| b >> 20;
    eprintln!(
        "[gc-histo] {} objects: cells {} MiB, objects {} MiB, with 8-byte classes {} MiB, compact layout {} MiB, 12-byte header {} MiB, 8-byte header and compact references {} MiB; large {} MiB",
        all.objects,
        mb(all.cells),
        mb(all.bytes),
        mb(all.step8),
        mb(all.compact),
        mb(all.header12),
        mb(all.header8),
        mb(space.large.values().filter(|l| l.marked).map(|l| l.size).sum())
    );
    let mut top: Vec<_> = by_class.into_iter().collect();
    top.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
    for (name, (n, b)) in top.iter().take(25) {
        eprintln!("[gc-histo]   {:>7} KiB {:>9} {}", b >> 10, n, name);
    }
}
