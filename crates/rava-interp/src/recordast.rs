//! 21.1 M4 CODEC-FLATTEN SCAN (RAVA_M4_CODEC_FLATTEN, default OFF). INERT recon for the Codec-tree
//! flattening lever: at codec construction, classify the Mojang DFU combinator vocabulary
//! (RecordCodecBuilder / MapCodec / KeyDispatchCodec) into flatten-candidates vs opaque (custom
//! Java-lambda dispatch); at decode, measure the runtime combinator-tree DEPTH (the "50-floor" stack
//! a flat native function would collapse). Counters + a Rust `CodecNode` AST scaffold only — never a
//! value or control-flow touch ⇒ hashes converge bit-for-bit.
//! 24.2 RECORD-AST OBSERVER (RAVA_JIT_RECORDAST_PROBE, inert). Phase A of Codec-Tree Flattening: at
//! DECODE time, observe each record MapCodec's decode and build a `RecordAst` (ordered fields + kinds)
//! keyed by the decoder's STABLE class identity (arena class_id / interned Ref name — survives GC,
//! unlike the 22.1 construction addr). Measures flatten-worthiness (primitive-field ratio + decode-
//! frame coverage) BEFORE the Cranelift generator. Fields captured via a thread-local stack: a record
//! decode pushes a frame; each nested FieldDecoder.decode records (name, kind) into the top; the frame
//! finalizes into the registry on exit. Lock-free on the per-field path (thread-local Vec); the Mutex
//! registry is touched only on record-decode enter/exit (~thousands/DecodeProbe, not the hot path).
//! No guest allocation, no GC-ref mutation ⇒ bit-exact.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_RECORDAST_PROBE").is_some())
}

// Field kinds (flatten-worthiness): 0=Primitive, 1=List, 2=Record, 3=Dispatch, 4=Opaque.
pub const PRIMITIVE: u8 = 0;
pub const LIST: u8 = 1;
pub const RECORD: u8 = 2;
pub const DISPATCH: u8 = 3;
pub const OPAQUE: u8 = 4;

pub struct RecordAst {
    pub fields: Vec<(String, u8)>, // (field name, kind) in decode order
    pub frames: u64,               // decode frames this record covered
}

// KEY = structural fingerprint (FNV of the field-name sequence). RecordCodecBuilder$2/MapCodecCodec
// are SHARED classes across ALL records (class-ptr would collapse everything — the 22.1 singleton
// trap), and the instance addr dies on GC (22.1). The field-name sequence is the stable, self-
// contained record identity, computed live during observation.
static REGISTRY: Mutex<Option<HashMap<u64, RecordAst>>> = Mutex::new(None);
static TOTAL_DECODE_FRAMES: AtomicU64 = AtomicU64::new(0);
static RECORD_DECODE_FRAMES: AtomicU64 = AtomicU64::new(0);

struct ObsFrame {
    fields: Vec<(String, u8)>,
    input: crate::Value, // 24.3: the decode `input` (ops target) for the shadow-flat compare
}
thread_local! {
    static STACK: RefCell<Vec<ObsFrame>> = const { RefCell::new(Vec::new()) };
}

/// 24.3: if the TOP record frame is a flatten target (ALL fields Primitive), return its (input,
/// fields) so the caller can run the shadow-flat compare against the DFU result. Read-only clone.
pub fn top_flat_target() -> Option<(crate::Value, Vec<(String, u8)>)> {
    STACK.with(|s| {
        let b = s.borrow();
        let f = b.last()?;
        if !f.fields.is_empty() && f.fields.iter().all(|(_, k)| *k == PRIMITIVE) {
            Some((f.input.clone(), f.fields.clone()))
        } else {
            None
        }
    })
}

/// A record MapCodec's decode (the flatten unit). RecordCodecBuilder$2 (the record Codec) AND its
/// $Instance$N (the Products.apply MapDecoder that actually reads the N fields) both count — the
/// innermost pushed frame collects the fields; an empty outer frame finalizes to nothing.
pub fn is_record_decode(class: &str, name: &str) -> bool {
    name == "decode"
        && (class.starts_with("com/mojang/serialization/codecs/RecordCodecBuilder")
            || class == "com/mojang/serialization/MapCodec$MapCodecCodec")
}

/// Count one is_decode frame (any serialization decode) for the coverage denominator.
pub fn note_decode_frame() {
    TOTAL_DECODE_FRAMES.fetch_add(1, Relaxed);
}

/// Enter a record decode: push an observation frame. The record's identity (its field-name
/// fingerprint) isn't known until the fields are seen, so keying happens at finalize (drop).
pub fn enter_record(input: crate::Value) -> RecordGuard {
    let n = RECORD_DECODE_FRAMES.fetch_add(1, Relaxed) + 1;
    // Periodic dump so a NON-exiting run (server boot never exits after Done) still yields the map.
    // Early milestones catch the registry-load phase before the long worldgen / any timeout.
    if n == 3_000 || n == 20_000 || n == 100_000 || n % 500_000 == 0 {
        dump();
    }
    STACK.with(|s| s.borrow_mut().push(ObsFrame { fields: Vec::new(), input }));
    RecordGuard
}

/// Record a field decode (name, kind) into the current (top) record frame.
pub fn on_field(name: String, kind: u8) {
    STACK.with(|s| {
        if let Some(f) = s.borrow_mut().last_mut() {
            if f.fields.len() < 64 {
                f.fields.push((name, kind));
            }
        }
    });
}

/// Are we inside a record decode (so a FieldDecoder.decode is a direct field to record)?
pub fn in_record() -> bool {
    STACK.with(|s| !s.borrow().is_empty())
}

fn fingerprint(fields: &[(String, u8)]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for (n, k) in fields {
        for b in n.bytes().chain(std::iter::once(*k)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

pub struct RecordGuard;
impl Drop for RecordGuard {
    fn drop(&mut self) {
        let Some(f) = STACK.with(|s| s.borrow_mut().pop()) else { return };
        if f.fields.is_empty() {
            return; // a wrapper decode with no direct fields (e.g. MapCodecCodec over a non-record)
        }
        let fp = fingerprint(&f.fields);
        let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        match m.get_mut(&fp) {
            Some(a) => a.frames += 1,
            None => {
                m.insert(fp, RecordAst { fields: f.fields, frames: 1 });
            }
        }
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else {
        eprintln!("[RECORD-AST] no record decodes observed");
        return;
    };
    let total_frames = TOTAL_DECODE_FRAMES.load(Relaxed).max(1);
    let record_frames = RECORD_DECODE_FRAMES.load(Relaxed);
    let (mut all_fields, mut prim_fields) = (0u64, 0u64);
    for a in m.values() {
        for &(_, k) in &a.fields {
            all_fields += 1;
            if k == PRIMITIVE {
                prim_fields += 1;
            }
        }
    }
    eprintln!(
        "[RECORD-AST] unique record-codec classes = {}  | record decode frames = {} ({}% of {} total decode frames)",
        m.len(),
        record_frames,
        record_frames * 100 / total_frames,
        total_frames
    );
    let pratio = if all_fields > 0 { prim_fields * 1000 / all_fields } else { 0 };
    eprintln!(
        "[RECORD-AST] fields across records = {all_fields}  primitive = {prim_fields} ({}.{}%) ⇒ flatten-worthiness",
        pratio / 10,
        pratio % 10
    );
    // The heaviest records (by decode-frame coverage) — the flatten targets. The field list IS the
    // record identity (e.g. a biome shows [temperature downfall effects ...]).
    let mut rows: Vec<&RecordAst> = m.values().collect();
    rows.sort_by(|a, b| b.frames.cmp(&a.frames));
    let kname = |k: u8| match k { PRIMITIVE => "Prim", LIST => "List", RECORD => "Rec", DISPATCH => "Disp", _ => "Opaq" };
    for a in rows.iter().take(15) {
        let np = a.fields.iter().filter(|(_, k)| *k == PRIMITIVE).count();
        let flds: Vec<String> = a.fields.iter().take(20).map(|(n, k)| format!("{n}:{}", kname(*k))).collect();
        eprintln!(
            "[RECORD-AST]   frames={:>7} fields={:>2} prim={:>2}  {}",
            a.frames, a.fields.len(), np, flds.join(" ")
        );
    }
}
