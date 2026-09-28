//! Accumulating a census over the object table.

use crate::heap::*;

impl HeapCensus {
    pub const ARR_KIND_NAMES: [&'static str; 6] =
        ["byte", "int", "long", "float", "double", "ref"];

    /// A structured, sorted report. Every line carries its share of `total`, so the
    /// biggest component is unambiguous — the whole point of the exercise.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        let pct = |b: usize| {
            if self.total == 0 { 0.0 } else { b as f64 * 100.0 / self.total as f64 }
        };
        let mut s = String::new();
        let _ = writeln!(s, "[CENSUS] ==== live Rust-side heap breakdown ====");
        let _ = writeln!(
            s,
            "[CENSUS] live_objects={} dead_slots={} obj_slots={} field_slots={} spilled_objs={} ({:.1}%)",
            self.live_objects,
            self.dead_slots,
            self.obj_slots,
            self.field_slots,
            self.spilled_objects,
            if self.live_objects == 0 { 0.0 }
            else { self.spilled_objects as f64 * 100.0 / self.live_objects as f64 }
        );
        let _ = writeln!(
            s,
            "[CENSUS] live_maps={} map_entries={} live_arrays={} lambdas={}",
            self.live_maps, self.map_entry_count, self.live_arrays, self.lambda_count
        );
        let mut rows: Vec<(&str, usize)> = vec![
            ("obj.struct (Object)", self.obj_struct),
            ("obj.box_tax (malloc)", self.obj_box_tax),
            ("obj.table (Vec<Box>)", self.obj_table),
            ("obj.desc_table", self.obj_desc_table),
            ("obj.DEAD structs", self.dead_struct),
            ("field blob (1 alloc/obj)", self.field_blob_bytes),
            ("lambda", self.lambda_bytes),
            ("lambda.site (shared)", self.lambda_site_bytes),
            ("map.entries Vec", self.map_entries_bytes),
            ("map.index HashMap", self.map_index_bytes),
            ("map.key Box<str>", self.map_key_str_bytes),
            ("array payload", self.arr_payload_bytes),
            ("array obj table", self.arr_obj_table),
            ("array desc table", self.arr_desc_table),
            ("field_box side", self.field_box_bytes),
            ("array_box side", self.array_box_bytes),
            ("free-handle Vecs", self.free_handle_bytes),
            ("intern tables", self.intern_bytes),
        ];
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for (name, b) in &rows {
            let _ = writeln!(s, "[CENSUS] {:>9.1}MB {:>5.1}%  {name}", mb(*b), pct(*b));
        }
        let [(obj_len, obj_cap), (arr_len, arr_cap), (map_len, map_cap)] = self.free_handles;
        let _ = writeln!(
            s,
            "[CENSUS] -- free handles: obj={obj_len}/{obj_cap} arr={arr_len}/{arr_cap} map={map_len}/{map_cap} (len/cap), retained={:.1}MB",
            mb(self.free_handle_bytes)
        );
        let _ = writeln!(s, "[CENSUS] {:>9.1}MB 100.0%  TOTAL (Rust-side)", mb(self.total));
        let _ = writeln!(
            s,
            "[CENSUS] -- sizeof: Object={}B ObjDesc={}B Value={}B (Value,Value)={}B (MapKey,usize)={}B Lambda={}B LambdaSite={}B",
            std::mem::size_of::<Object>(),
            std::mem::size_of::<ObjDesc>(),
            std::mem::size_of::<Value>(),
            std::mem::size_of::<(Value, Value)>(),
            std::mem::size_of::<(MapKey, usize)>(),
            std::mem::size_of::<Lambda>(),
            std::mem::size_of::<LambdaSite>(),
        );
        for (i, name) in Self::ARR_KIND_NAMES.iter().enumerate() {
            let (n, b) = self.arr_payload[i];
            if n > 0 {
                let _ = writeln!(s, "[CENSUS] -- array[{name}] {n} arrays {:.1}MB", mb(b));
            }
        }
        // INLINE-≤8 GATE: fraction of live arrays that would fit an inline buffer, and what
        // moving them costs/saves. Spine grows by (extra sizeof per slot) × arr_slots; payload
        // saved = the ≤8 arrays' current separate Vec allocations.
        let le8_pct = if self.live_arrays == 0 { 0.0 } else { self.arr_le8_count as f64 * 100.0 / self.live_arrays as f64 };
        let _ = writeln!(
            s,
            "[CENSUS] -- inline-≤8 gate: {}/{} live arrays are ≤8 elems ({:.1}%), {} of them empty; their payload={:.1}MB. sizeof(ArrayObj)={}B (spine={:.1}MB @ {} slots)",
            self.arr_le8_count, self.live_arrays, le8_pct, self.arr_le0_count, mb(self.arr_le8_payload),
            std::mem::size_of::<ArrayObj>(), mb(self.arr_obj_table), self.arr_slots,
        );
        // REF-vs-PRIM decomposition: alloc = cap·elem+tax; used = len·elem; unused = slack capacity.
        let (rc, ra, ru, run_) = self.arr_ref;
        let (pc, pa, pu, pun) = self.arr_prim;
        let _ = writeln!(
            s,
            "[CENSUS] -- array REF:  {rc} arrays alloc={:.1}MB used={:.1}MB unused-cap={:.1}MB",
            mb(ra), mb(ru), mb(run_)
        );
        let _ = writeln!(
            s,
            "[CENSUS] -- array PRIM: {pc} arrays alloc={:.1}MB used={:.1}MB unused-cap={:.1}MB",
            mb(pa), mb(pu), mb(pun)
        );
        // Length histogram — where the array WEIGHT actually lives by size class.
        const HIST_NAMES: [&str; 5] = ["len=0", "1..=4", "5..=8", "9..=64", ">64"];
        for (i, name) in HIST_NAMES.iter().enumerate() {
            let (n, b) = self.arr_len_hist[i];
            let cpct = if self.live_arrays == 0 { 0.0 } else { n as f64 * 100.0 / self.live_arrays as f64 };
            let _ = writeln!(
                s,
                "[CENSUS] -- array len[{name:>6}]: {n} arrays ({cpct:.1}%) {:.1}MB",
                mb(b)
            );
        }
        // ── MAP decomposition (FlatMap justification) ──
        // entries: real pairs vs allocated slots; index: usable slots vs the hashbrown alloc.
        let entry_alloc = self.map_entry_cap * std::mem::size_of::<(Value, Value)>();
        let _ = writeln!(
            s,
            "[CENSUS] -- map {} live: entries len={} cap={} ({:.1}MB alloc, {:.1}MB SLACK) | index slots={} ({:.1}MB alloc) | keys {:.1}MB",
            self.live_maps, self.map_entry_count, self.map_entry_cap,
            mb(entry_alloc), mb(self.map_entry_slack_bytes),
            self.map_index_slots, mb(self.map_index_bytes), mb(self.map_key_str_bytes)
        );
        // Fill histogram by real entry count — the FlatMap tiebreaker (dominant at 0..=4 ⇒ FlatMap wins).
        const MAP_FILL_NAMES: [&str; 5] = ["0", "1", "2..=4", "5..=8", ">8"];
        for (i, name) in MAP_FILL_NAMES.iter().enumerate() {
            let (n, b) = self.map_fill_hist[i];
            let cpct = if self.live_maps == 0 { 0.0 } else { n as f64 * 100.0 / self.live_maps as f64 };
            let _ = writeln!(
                s,
                "[CENSUS] -- map fill[{name:>5}]: {n} maps ({cpct:.1}%) {:.1}MB",
                mb(b)
            );
        }
        // ── LAMBDA captured-context decomposition ──
        let dups = self.lambda_capturing.saturating_sub(self.lambda_capture_distinct);
        let duppct = if self.lambda_capturing == 0 { 0.0 } else { dups as f64 * 100.0 / self.lambda_capturing as f64 };
        let _ = writeln!(
            s,
            "[CENSUS] -- lambda captured: {} of {} lambdas capture, {:.1}MB captured buffers; DUPLICATE captures (same site+values) {dups}/{} ({duppct:.1}%) ⇒ dedup would reclaim {:.1}MB",
            self.lambda_capturing, self.lambda_count, mb(self.lambda_captured_bytes),
            self.lambda_capturing, mb(self.lambda_dup_bytes)
        );
        const CAP_NAMES: [&str; 4] = ["0(pure)", "1", "2..=4", ">4"];
        for (i, name) in CAP_NAMES.iter().enumerate() {
            let (n, b) = self.lambda_cap_hist[i];
            let cpct = if self.lambda_count == 0 { 0.0 } else { n as f64 * 100.0 / self.lambda_count as f64 };
            let _ = writeln!(
                s,
                "[CENSUS] -- lambda cap[{name:>7}]: {n} lambdas ({cpct:.1}%) {:.1}MB",
                mb(b)
            );
        }
        s
    }
}
