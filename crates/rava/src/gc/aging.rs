//! Aging of promoted objects: how many young collections a survivor stays young for (HotSpot's
//! tenuring threshold, adapted to how much of each promotion survives), and the aging ranges each
//! young collection keeps, packed for mixed pages (`mixed.rs`).

use super::mark::in_ranges;
use super::space::Space;

/// The tenuring threshold after a young collection in which `kept` of the `given` bytes young
/// again survived (HotSpot adapts its own to what survives too): when nearly all of them do,
/// as a program builds its long-lived data, retracing them collection after collection only
/// costs (All the Mods 10's start traced 4-16 million such objects per young collection), so
/// they get one more chance only; once many die again, the full threshold returns.
pub(super) fn tenure_after(tenure: u8, given: usize, kept: usize) -> u8 {
    if given == 0 {
        return tenure;
    }
    match kept * 100 / given {
        90.. => SHORT_TENURE,
        0..75 => TENURE,
        _ => tenure,
    }
}

/// The threshold while nearly every recent promotion survives (`tenure_after`).
const SHORT_TENURE: u8 = 2;

/// Young collections per one that gives promotions their chance under `SHORT_TENURE`.
const PROBE: u8 = 8;

/// Young collections an object survives before it is old for good (HotSpot's tenuring
/// threshold). Promoted after its first, it counts as young again in the next ones: All the
/// Mods 10 still used 3-6 MiB of each young collection's 4-8 MiB of promotions one collection
/// later, objects that died seconds on and, old already, filled the old generation.
pub(super) const TENURE: u8 = 8;

/// The aging cells after a young collection: the survivors among `aging`, one collection older
/// (until `TENURE`), and the cells this one promoted into (`promoted`, sorted). Survivors a
/// once-only root holds (`pinned`, sorted) are old for good.
pub(super) fn age(space: &mut Space, aging: &[(usize, usize, u8)], promoted: &[(usize, usize)], pinned: &[usize]) -> Vec<(usize, usize, u8)> {
    // While nearly every promotion survives, retracing them costs as much as the rest of a young
    // collection (chunk generation: 4.5 ms of 8.7); they get their chance in one young
    // collection of `PROBE` only, enough to see whether they still survive (`tenure_after`).
    let chance = space.tenure != SHORT_TENURE || {
        space.since_chance += 1;
        space.since_chance >= PROBE
    };
    if chance {
        space.since_chance = 0;
    }
    let mut out: Vec<(usize, usize, u8)> = if chance { promoted.iter().map(|&(s, e)| (s, e, 1)).collect() } else { Vec::new() };
    for &(start, end, years) in aging {
        let tenured = years + 1 >= space.tenure;
        let Some(index) = space.page_index(start) else { continue };
        let page = space.page_addr(index);
        let Some(meta) = space.pages[index].as_mut() else { continue };
        if meta.is_mixed() {
            // An aging range held objects alive at the last collection, packed: the gaps
            // between its survivors now are objects that died since, space later copies reuse
            // (`Space::mixed_run`). Survivors next to each other share a range.
            let (mut run, mut free, mut holes) = (None::<(usize, usize)>, start, Vec::new());
            super::mixed::each(&meta.marks, page, start, end, |at, size| {
                if at > free {
                    holes.push((free, at));
                }
                free = at + size;
                if tenured || in_ranges(promoted, at) || pinned.binary_search(&at).is_ok() {
                    return;
                }
                match &mut run {
                    Some((_, e)) if *e == at => *e = at + size,
                    _ => {
                        if let Some((s, e)) = run.replace((at, at + size)) {
                            out.push((s, e, years + 1));
                        }
                    }
                }
            });
            if end > free {
                holes.push((free, end));
            }
            if let Some((s, e)) = run {
                out.push((s, e, years + 1));
            }
            super::mixed::add_holes(meta, page, &holes);
            continue;
        }
        if tenured {
            continue;
        }
        let size = meta.cell_size as usize;
        let mut run: Option<usize> = None;
        for cell in meta.cell_of(start - page)..meta.cell_of(end - page) {
            let at = page + cell * size;
            // Survivors, less the cells a copy of this collection took (those are age 1) and
            // those pinned.
            let live = meta.marks[cell / 64] & (1u64 << (cell % 64)) != 0 && !in_ranges(promoted, at) && pinned.binary_search(&at).is_err();
            match (live, run) {
                (true, None) => run = Some(at),
                (false, Some(s)) => {
                    out.push((s, at, years + 1));
                    run = None;
                }
                _ => {}
            }
        }
        if let Some(s) = run {
            out.push((s, end, years + 1));
        }
    }
    out.sort_unstable();
    out
}

