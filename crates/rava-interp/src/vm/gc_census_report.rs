//! Printing the cell census: the peak snapshot and the per-target report.
//!
//! Split from the gathering pass because it is pure output over tallies already collected.

use crate::*;
use std::io::Write as _;

pub(crate) type CensusRows = Vec<(std::sync::Arc<str>, [u64; 4])>;

#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cell_census_report(
    vm: &mut Vm,
    targets: &std::collections::HashMap<String, u64>,
    target_ids: &std::collections::HashMap<String, std::collections::HashSet<u32>>,
    arrays: &std::collections::HashMap<String, u64>,
    boxes: &std::collections::HashMap<&'static str, u64>,
    h_by_holder: &std::collections::HashMap<(u32, usize, String), u64>,
    arr_by_holder: &std::collections::HashMap<(u32, usize), u64>,
    rows: &CensusRows,
    arr_synth: u64,
    arr_guest: u64,
    h_synth: u64,
    h_guest: u64,
    max_handle: u32,
    max_array: u32,
    max_box: usize,
    tot: [u64; 4],
    blocking: u64,
) {
    let mut peak = CELL_CENSUS_PEAK.lock().unwrap_or_else(|e| e.into_inner());
    if blocking > *peak {
        *peak = blocking;
        let all: u64 = tot.iter().sum();
        let pct = if all > 0 { tot[0] as f64 * 100.0 / all as f64 } else { 100.0 };
        eprintln!(
            "[CELL-CENSUS] ref cells {all}: 0b00 {} ({pct:.2}% bare-ready) | handle {} | array {} | box {} => {blocking} BLOCK the flip",
            tot[0], tot[1], tot[2], tot[3]
        );
        // TENURED FLOOR beside the total. The walk strides the bump regions, so the
        // line above includes cells of arena objects that are already garbage; a
        // tenured object has survived a collection, so this pair BRACKETS the truth
        // instead of presenting the upper bound as exact.
        {
            let t = crate::nextgen::last_tenured_cells();
            let tall: u64 = t.iter().sum();
            let tblock = t[1] + t[2] + t[3];
            let tpct = if tall > 0 { t[0] as f64 * 100.0 / tall as f64 } else { 100.0 };
            eprintln!(
                "[CELL-CENSUS] TENURED only (definitely-live floor) {tall} cells: \
                 0b00 {} ({tpct:.2}% bare-ready) | {tblock} block",
                t[0]
            );
        }
        // Which enum classes actually took which birth route (RAVA_ENUM_BIRTH_CENSUS).
        // Printed here because the cell census already runs on a path a killed boot
        // reaches, and the two readings only mean something side by side: the target
        // histogram below says which classes still hold blocking cells, and this says
        // whether those same classes were arena-born.
        crate::enum_birth::dump_names();
        crate::comparator_plan::cmp_plan_census_report();
        crate::natives::served_census::report();
        {
            let m = crate::map::dispatch::mapget_census::report();
            if !m.is_empty() {
                eprintln!("{m}");
            }
        }
        // And whether the classes that still hold blocking cells reach Heap::alloc, which
        // always mints a handle (RAVA_ALLOC_HISTO + RAVA_ALLOC_HISTO_FIND).
        crate::allochisto::find();
        for (cls, c) in rows.iter().take(12) {
            if c[1] + c[2] + c[3] == 0 {
                break;
            }
            eprintln!(
                "[CELL-CENSUS]   handle {:>8} array {:>7} box {:>7} | ok {:>8}  {cls}",
                c[1], c[2], c[3], c[0]
            );
        }
        // Copy-order LOCALITY: after the bare-deref flip a ref load is
        // pure cache behaviour, so the parent→child distance the Cheney
        // breadth-first evacuation leaves behind is the residual cost.
        let loc = crate::nextgen::edge_locality_census(crate::nextgen::registry_len());
        let ltot: u64 = loc.iter().sum();
        if ltot > 0 {
            let names = ["=line", "<2", "<4", "<8", "<16", "<64", "<256", "<1k", "<4k", ">=4k"];
            let parts: Vec<String> = names
                .iter()
                .zip(loc.iter())
                .map(|(n, c)| format!("{n} {:.1}%", *c as f64 * 100.0 / ltot as f64))
                .collect();
            eprintln!("[CELL-CENSUS] edge locality ({ltot} arena->arena edges, 64B lines): {}", parts.join(" | "));
        }
        let mut tgt: Vec<(String, u64)> = targets.iter().map(|(k, v)| (k.clone(), *v)).collect();
        tgt.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (cls, n) in tgt.iter().take(12) {
            let d = target_ids.get(cls).map_or(0, |s| s.len());
            // WHY is this class a handle? `by_name` = its OWN name matched the
            // collection substring heuristic; `by_anc` = it genuinely inherits
            // native-backed state from an ancestor. A class that is only `by_name`
            // is handled for what it is CALLED, not for what it holds — and the
            // tree's own test asserts fastutil array maps have no native contract,
            // so the largest blocker may be in exactly that group.
            let by_name = crate::opcodes::birth_native_backed_alloc(cls);
            let by_anc = vm.birth_native_backed_ancestor(cls)
                && !vm.overrides_collection_backing(cls);
            let why = match (by_name, by_anc) {
                (true, true) => "name+ancestor",
                (true, false) => "NAME ONLY",
                (false, true) => "ancestor",
                (false, false) => "neither (born handle for another reason)",
            };
            eprintln!(
                "[CELL-CENSUS] target {n:>9} cells over {d:>7} distinct objs  {cls}  [{why}]"
            );
        }
        let mut arr: Vec<(String, u64)> = arrays.iter().map(|(k, v)| (k.clone(), *v)).collect();
        arr.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (el, n) in arr.iter().take(10) {
            eprintln!("[CELL-CENSUS] array-elem {n:>9}  {el}");
        }
        eprintln!(
            "[CELL-CENSUS] array holders: {arr_synth} in rava's SYNTHETIC collection \
             storage ($elems/$sb/$buf) | {arr_guest} in a GUEST-declared field",
        );
        eprintln!(
            "[CELL-CENSUS] handle holders: {h_synth} SYNTHETIC (never bare-dereffed, \
             NOT blockers) | {h_guest} guest-declared (the real residue)",
        );
        // THE NUMBER THAT DECIDES WHAT IS LEFT. A handle cell only blocks the flip if
        // some site would BARE-DEREF it, and `descriptor_keeps_tag` is exactly the
        // emitter's rule for that: a field declared as an array, as `Object`, or as a
        // native-backed type keeps its runtime test and can hold a handle forever.
        // Only the rest — fields declared as ordinary jar classes — must reach 0b00.
        {
            let (mut kept, mut must, mut unres) = (0u64, 0u64, 0u64);
            let mut would_keep_with_ancestor = 0u64;
            let mut would_keep_if_abstract = 0u64;
            let mut would_keep_combined = 0u64;
            let mut would_keep_stable = 0u64;
            let mut stable_residue: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            // What the genuinely-blocking cells POINT AT. This is the actual work
            // list — everything else in the census is already handled by the emitter.
            let mut must_targets: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            for (&(cid, slot, ref tgt), &n) in h_by_holder.iter() {
                let desc = crate::nextgen::class_layout(cid)
                    .and_then(|(_, l)| l.get(slot).map(|&(f, _)| f))
                    .and_then(|f| vm.heap.field_name(f))
                    .and_then(|nm: String| {
                        let (decl, fname) = nm.split_once('#')?;
                        let c = vm.loader.get_or_load(decl).ok()?;
                        c.file
                            .fields
                            .iter()
                            .find(|f| f.name == fname)
                            .map(|f| f.descriptor.to_string())
                    });
                // PRICING an extension before building it. `descriptor_keeps_tag`
                // asks only about the declared type's NAME. A field declared as a
                // type whose ANCESTOR is native-backed also always holds a handle —
                // that type can never be arena-born — so keeping the tag there is
                // both correct and necessary, and those cells were never blockers.
                // The emitter cannot ask this today (no `Vm` at compile time, and
                // `background_jit` ships jobs to another thread so a closure cannot
                // cross); the census can, which prices the plumbing before it exists.
                // A field rava INJECTED ($val, $bits, $elems) has no entry in the
                // class file, so the descriptor lookup above returns None and the
                // cell was counted as `unresolved` — doubt this same census had
                // already dispelled two lines earlier, where those very cells are
                // tallied as SYNTHETIC and explicitly NOT blockers because nothing
                // bare-derefs them. What identified it: the two totals matched to
                // the cell (1,735 handle, 9,425 array), which no coincidence explains.
                let is_synth = crate::nextgen::class_layout(cid)
                    .and_then(|(_, l)| l.get(slot).map(|&(f, _)| f))
                    .and_then(|f| vm.heap.field_name(f))
                    .is_some_and(|n: String| n.starts_with('$') || n.contains("#$"));
                let anc_keeps = desc
                    .as_deref()
                    .and_then(|d| d.strip_prefix('L').and_then(|x| x.strip_suffix(';')))
                    .is_some_and(|c| {
                        vm.birth_native_backed_ancestor(c)
                            && !vm.overrides_collection_backing(c)
                    });
                // The classifier must mirror what the EMITTER now does, which is
                // `Vm::tagged_field_indices`: the descriptor rule OR the ancestor
                // rule. Leaving it on the descriptor rule alone kept reporting 34,733
                // after the emitter had already stopped blocking on 14,541 of them —
                // the sixth time in this arc a metric disagreed with its mechanism,
                // and the first where the mechanism had moved and the metric had not.
                match desc.as_deref() {
                    Some(d) if crate::jit::descriptor_keeps_tag_pub(d) || anc_keeps => {
                        kept += n
                    }
                    Some(_) => {
                        must += n;
                        // Counted INSIDE this arm only. Incrementing before the match
                        // tallied cells the emitter already keeps, giving 116,856
                        // against a `must` of 34,733 and a saturating "residue would
                        // be 0" — a number too good to be true, which is what made it
                        // visible. The question is only about cells that BLOCK today.
                        if anc_keeps {
                            would_keep_with_ancestor += n;
                        }
                        // PRICING a third rule before building it. A field declared
                        // as an INTERFACE or an ABSTRACT class can legally hold ANY
                        // implementor, and one of those may be native-backed — so
                        // such a site can never bare-deref and its cells were never
                        // blockers either. ACC_INTERFACE/ACC_ABSTRACT is a pure
                        // class-file property: no names, no workload, no-adapt clean.
                        // The MUST-MIGRATE list names TARGETS, not declared types,
                        // which is what suggested it: cells pointing at LinkedHashMap
                        // sit in fields declared `Map`.
                        let dcls = desc.as_deref().and_then(|d| {
                            d.strip_prefix('L').and_then(|x| x.strip_suffix(';'))
                        });
                        if dcls
                            .and_then(|c| vm.loader.get_or_load(c).ok())
                            .is_some_and(|c| c.file.access_flags & 0x0600 != 0)
                        {
                            would_keep_if_abstract += n;
                        }
                        // The COMBINED conservative rule, priced as one number because
                        // that is the only number that decides anything: the flip ships
                        // when the residue is ZERO, not when it is small. Keeping a tag
                        // is always sound -- it costs a test, never correctness -- so
                        // every clause here only has to be a reason the declared type
                        // might not be arena-born, not a proof that it never is:
                        //   * interface/abstract -- any implementor, one may be native
                        //   * no complete layout chain -- the birth route itself
                        //     refuses exactly this case (`class_layout_cache` miss),
                        //     so instances of it are handles by construction
                        //   * native-backed by name or ancestor -- the existing rules
                        let keeps = dcls.is_some_and(|c| {
                            let abstract_like = vm
                                .loader
                                .get_or_load(c)
                                .ok()
                                .is_some_and(|l| l.file.access_flags & 0x0600 != 0);
                            let no_layout = !vm.runtime_shared().class_layout_contains(c);
                            abstract_like || no_layout || anc_keeps
                        });
                        if keeps {
                            would_keep_combined += n;
                        }
                        // The STABLE subset, split out because one clause above is not
                        // a safe basis for an emit decision even though it is sound to
                        // KEEP a tag. `class_layout_cache` membership VARIES WITH TIME:
                        // a class not yet cached when a method compiles may become
                        // arena-birthable later, and -- the direction that bites -- a
                        // class already cached has instances born BEFORE it was, which
                        // are handles. A site compiled after that point would bare-deref
                        // one. Soundness has to rest on properties that can never change:
                        // interface/abstract, a native-backed name or ancestor, or no
                        // loadable class file at all. If this covers the residue too,
                        // the time-varying clause is not needed.
                        // `has_native_backed_ancestor`, NOT the birth-guarded variant.
                        // The two ask different questions and only one of them is this
                        // one. BIRTH asks "may I put this in the arena?" and must be
                        // narrow, which is why RAVA_ARENA_BIRTH_NS_GUARD tightened its
                        // name test so jar classes stop being force-handled. KEEP-TAG
                        // asks "might this EVER be a handle?" and must be wide. Reusing
                        // the narrowed predicate here let com/google/.../ImmutableSortedMap
                        // through -- a class rava's own natives construct, hence always
                        // a handle -- and it alone was 1,171 of the 1,208 residue.
                        // Read-only transitive walk over supers AND interfaces. The
                        // name predicates cannot answer this: guava's
                        // ImmutableSortedMap is loadable, concrete, and matches no
                        // native name, yet every instance is a handle — because the
                        // cells point at LinkedHashMap, i.e. rava's native map layer
                        // ANSWERS its construction and hands back a handle. What is
                        // common to every such class is not its name but the interface
                        // it implements, which is exactly what the native layer keys
                        // on. `implements_iface` needs `&mut self`, so the walk is
                        // local here rather than a borrow fight in a diagnostic.
                        let serves_native_iface = |start: &str| -> bool {
                            let mut queue = vec![start.to_string()];
                            let mut seen: Vec<String> = Vec::new();
                            while let Some(cn) = queue.pop() {
                                if cn == "java/util/Map" || cn == "java/util/Collection" {
                                    return true;
                                }
                                if seen.iter().any(|s| *s == cn) || seen.len() > 64 {
                                    continue;
                                }
                                seen.push(cn.clone());
                                if let Ok(l) = vm.loader.get_or_load(&cn) {
                                    queue.extend(l.file.interfaces.iter().cloned());
                                    if let Some(s) = l.file.super_class.clone() {
                                        queue.push(s);
                                    }
                                }
                            }
                            false
                        };
                        let stable = dcls.is_some_and(|c| {
                            match vm.loader.get_or_load(c) {
                                Ok(l) => {
                                    l.file.access_flags & 0x0600 != 0
                                        || vm.has_native_backed_ancestor(c)
                                        || serves_native_iface(c)
                                }
                                Err(_) => true,
                            }
                        });
                        if stable {
                            would_keep_stable += n;
                        } else if let Some(c) = dcls {
                            // The population no static rule can close: the DECLARED type
                            // is concrete, loadable and arena-birthable, yet the cell
                            // holds a handle -- so instances of that one class are MIXED.
                            // Keyed by declared type, not by target, because the target
                            // list cannot name this: what has to become total is the
                            // BIRTH of the declared type.
                            *stable_residue.entry(c.to_string()).or_insert(0) += n;
                        }
                        // Key the dead-handle bucket by its HOLDER, not its target:
                        // "points at something dead" names nothing, "held in
                        // Optional.$val" names the bug. The isolation said the
                        // Optional migration causes these; this says whether the
                        // cells are actually in Optionals.
                        let key = if tgt.starts_with('?') {
                            let holder = crate::nextgen::class_layout(cid)
                                .map(|(n, _)| n.to_string())
                                .unwrap_or_else(|| format!("class#{cid}"));
                            format!("{tgt} held in {holder}[slot {slot}]")
                        } else {
                            tgt.clone()
                        };
                        *must_targets.entry(key).or_insert(0) += n;
                    }
                    None if is_synth => kept += n,
                    None => unres += n,
                }
            }
            eprintln!(
                "[CELL-CENSUS] handle cells the EMITTER already keeps tagged: {kept} | \
                 genuinely must reach 0b00: {must} | unresolved: {unres}",
            );
            eprintln!(
                "[CELL-CENSUS] of those, {would_keep_with_ancestor} sit in fields whose \
                 DECLARED type has a native-backed ancestor (would keep the tag if the \
                 emitter could ask) => residue would be {}",
                must.saturating_sub(would_keep_with_ancestor)
            );
            eprintln!(
                "[CELL-CENSUS] of the residue, {would_keep_if_abstract} sit in fields \
                 DECLARED as an interface or abstract class (can hold any implementor, \
                 so never bare-dereffable) => residue would be {}",
                must.saturating_sub(would_keep_if_abstract)
            );
            eprintln!(
                "[CELL-CENSUS] COMBINED conservative rule keeps {would_keep_combined} \
                 of the {must} => FLIP RESIDUE {} (zero = the flip is globally sound)",
                must.saturating_sub(would_keep_combined)
            );
            eprintln!(
                "[CELL-CENSUS] STABLE-ONLY rule (no time-varying clause) keeps \
                 {would_keep_stable} of the {must} => FLIP RESIDUE {}",
                must.saturating_sub(would_keep_stable)
            );
            let mut sr: Vec<(String, u64)> = stable_residue.into_iter().collect();
            sr.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            for (cls, n) in sr.iter().take(12) {
                eprintln!("[CELL-CENSUS]   MIXED-BIRTH {n:>8}  declared {cls}");
            }
            let mut mt: Vec<(String, u64)> = must_targets.into_iter().collect();
            mt.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            for (cls, n) in mt.iter().take(10) {
                eprintln!("[CELL-CENSUS]   MUST-MIGRATE {n:>8}  {cls}");
            }
        }
        // DECLARED-DESCRIPTOR split (row 80). A field declared `[I`/`[Ljava/...;` is
        // statically an array — the flip needs no tag test there, so its cells never
        // blocked. A field declared `Ljava/lang/Object;` CAN legally hold an array, so
        // those sites must keep the test and their cells are genuine blockers.
        {
            let (mut decl_array, mut decl_object, mut unresolved) = (0u64, 0u64, 0u64);
            for (&(cid, slot), &n) in arr_by_holder.iter() {
                let fid = crate::nextgen::class_layout(cid)
                    .and_then(|(_, l)| l.get(slot).map(|&(f, _)| f));
                let name = fid.and_then(|f| vm.heap.field_name(f));
                let desc = name.as_deref().and_then(|n| {
                    let (decl, fname) = n.split_once('#')?;
                    let c = vm.loader.get_or_load(decl).ok()?;
                    let d = c
                        .file
                        .fields
                        .iter()
                        .find(|f| f.name == fname)
                        .map(|f| f.descriptor.to_string());
                    d
                });
                // Same rule as the handle split: a name that resolves to no class-file
                // field is one rava injected, and `$elems` holds an array by
                // construction — the case that needs no tag test at all.
                let is_synth = name
                    .as_deref()
                    .is_some_and(|n| n.starts_with('$') || n.contains("#$"));
                match desc.as_deref() {
                    Some(d) if d.starts_with('[') => decl_array += n,
                    Some(_) => decl_object += n,
                    None if is_synth => decl_array += n,
                    None => unresolved += n,
                }
            }
            eprintln!(
                "[CELL-CENSUS] array cells by DECLARED field type: {decl_array} declared \
                 `[...` (never need the tag test) | {decl_object} declared as an object \
                 type (must keep it) | {unresolved} unresolved",
            );
        }
        let mut bx: Vec<(&str, u64)> = boxes.iter().map(|(k, v)| (*k, *v)).collect();
        bx.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        eprintln!(
            "[CELL-CENSUS] box content: {}",
            bx.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" | ")
        );
        let _ = std::io::stderr().flush();
    }
}
