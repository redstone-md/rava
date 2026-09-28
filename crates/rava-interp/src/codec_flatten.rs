//! codec_flatten.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M4_CODEC_FLATTEN").is_some())
}

/// Native AST of a Mojang codec tree — the "skeletal registry" a flat-machine-code generator will
/// consume. 21.1 defines the shape + classifies the vocabulary; field-population + codegen come
/// in the flattener increment. Opaque = a node whose body is a custom guest lambda (unflattenable).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum CodecNode {
    Struct(Vec<(String, Box<CodecNode>)>), // RecordCodecBuilder record: named fields → sub-codecs
    Field(String, Box<CodecNode>),          // fieldOf("name", sub)
    Primitive(String),                      // Codec.INT / STRING / BOOL / DOUBLE / … (PrimitiveCodec)
    List(Box<CodecNode>),                   // listOf()
    Dispatch(Box<CodecNode>),               // KeyDispatchCodec: key → sub-codec (dynamic; opaque body)
    Opaque(String),                         // custom lambda / unmodelled combinator (class name)
}

/// Target combinator ctors. MapCodec is abstract — its super-<init> fires for EVERY subclass, so
/// matching it catches all map codecs; RecordCodecBuilder/KeyDispatchCodec are the concrete kinds.
pub fn is_ctor_target(class: &str) -> bool {
    matches!(
        class,
        "com/mojang/serialization/codecs/RecordCodecBuilder"
            | "com/mojang/serialization/codecs/KeyDispatchCodec"
            | "com/mojang/serialization/MapCodec"
    )
}
/// KeyDispatchCodec carries custom guest lambdas (type/decoder/encoder) ⇒ NOT flattenable.
fn is_flattenable(class: &str) -> bool {
    class != "com/mojang/serialization/codecs/KeyDispatchCodec"
}
/// A decode call on the Mojang serialization layer (for the runtime tree-depth probe).
pub fn is_decode(class: &str, name: &str) -> bool {
    name == "decode" && class.starts_with("com/mojang/serialization")
}
/// N for com/mojang/datafixers/Products$P{N} (1..=16) — the record's field arity.
pub fn products_arity(class: &str) -> Option<u32> {
    let n = class.strip_prefix("com/mojang/datafixers/Products$P")?;
    n.parse::<u32>().ok().filter(|&k| (1..=16).contains(&k))
}

static CTORS: AtomicU64 = AtomicU64::new(0);
static FLATTENABLE: AtomicU64 = AtomicU64::new(0);
static OPAQUE: AtomicU64 = AtomicU64::new(0);
static MAX_DECODE_DEPTH: AtomicU64 = AtomicU64::new(0);
static BY_CLASS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static DEPTH_HIST: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);
thread_local! {
    static DDEPTH: Cell<u32> = const { Cell::new(0) };
}

/// One codec ctor observed (construction phase). Inert.
pub fn note_ctor(class: &str) {
    CTORS.fetch_add(1, Relaxed);
    if is_flattenable(class) {
        FLATTENABLE.fetch_add(1, Relaxed);
    } else {
        OPAQUE.fetch_add(1, Relaxed);
    }
    let mut g = BY_CLASS.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(class.to_string()).or_insert(0) += 1;
}

/// RAII around a `decode` call — the depth = combinator-tree nesting at this point (the "50-floor").
pub struct DecodeGuard;
impl Drop for DecodeGuard {
    fn drop(&mut self) {
        DDEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}
pub fn enter_decode() -> DecodeGuard {
    let d = DDEPTH.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    MAX_DECODE_DEPTH.fetch_max(d as u64, Relaxed);
    let mut g = DEPTH_HIST.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(d).or_insert(0) += 1;
    DecodeGuard
}

impl CodecNode {
    /// Compact one-line rendering for the dump log.
    pub fn render(&self) -> String {
        match self {
            CodecNode::Struct(fs) => {
                let inner: Vec<String> = fs.iter().map(|(n, c)| format!("{n}:{}", c.render())).collect();
                format!("Struct{{{}}}", inner.join(", "))
            }
            CodecNode::Field(n, c) => format!("Field({n}={})", c.render()),
            CodecNode::Primitive(p) => format!("Prim({p})"),
            CodecNode::List(c) => format!("List<{}>", c.render()),
            CodecNode::Dispatch(k) => format!("Dispatch(key={})", k.render()),
            CodecNode::Opaque(s) => {
                let short = s.rsplit('/').next().unwrap_or(s);
                format!("Opaque({short})")
            }
        }
    }
    /// Max structural depth of the built AST (0 for a leaf).
    pub fn depth(&self) -> u32 {
        match self {
            CodecNode::Primitive(_) | CodecNode::Opaque(_) => 0,
            CodecNode::Field(_, c) | CodecNode::List(c) | CodecNode::Dispatch(c) => 1 + c.depth(),
            CodecNode::Struct(fs) => 1 + fs.iter().map(|(_, c)| c.depth()).max().unwrap_or(0),
        }
    }
}

/// 21.2 Skeletal registry: each observed field-codec's built AST, keyed by the codec Java-object
/// address (Ref handle / arena ptr). Populated inert at construction; consumed by the flattener.
static REGISTRY: Mutex<Option<HashMap<u64, CodecNode>>> = Mutex::new(None);
static AST_BUILT: AtomicU64 = AtomicU64::new(0);
static AST_FULLY_STRUCTURAL: AtomicU64 = AtomicU64::new(0); // no Opaque node anywhere

/// Encapsulated lookup for the shadow interpreter: the built AST for a codec object
/// address, cloned out (no lock held by the caller). Inert — read-only on the registry.
pub fn ast_for(addr: u64) -> Option<CodecNode> {
    let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().and_then(|m| m.get(&addr).cloned())
}

pub fn register_ast(addr: u64, node: CodecNode) {
    AST_BUILT.fetch_add(1, Relaxed);
    if !node_has_opaque(&node) {
        AST_FULLY_STRUCTURAL.fetch_add(1, Relaxed);
    }
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new).insert(addr, node);
}
fn node_has_opaque(n: &CodecNode) -> bool {
    match n {
        CodecNode::Opaque(_) => true,
        CodecNode::Primitive(_) => false,
        CodecNode::Field(_, c) | CodecNode::List(c) | CodecNode::Dispatch(c) => node_has_opaque(c),
        CodecNode::Struct(fs) => fs.iter().any(|(_, c)| node_has_opaque(c)),
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let (c, f, o) = (
        CTORS.load(Relaxed),
        FLATTENABLE.load(Relaxed),
        OPAQUE.load(Relaxed),
    );
    // 21.2: built ASTs + a few sample field trees (the "one key codec" dump).
    eprintln!(
        "[CODEC-FLATTEN] native ASTs built = {}  (fully-structural, no Opaque = {})",
        AST_BUILT.load(Relaxed),
        AST_FULLY_STRUCTURAL.load(Relaxed)
    );
    {
        let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_ref() {
            // Deepest / most-structural trees first (the interesting ones — biome/dimension fields).
            let mut v: Vec<&CodecNode> = m.values().collect();
            v.sort_by(|a, b| b.depth().cmp(&a.depth()));
            for node in v.iter().take(16) {
                eprintln!("[CODEC-FLATTEN]   AST d{}: {}", node.depth(), node.render());
            }
        }
    }
    eprintln!(
        "[CODEC-FLATTEN] combinator ctors observed = {c}  (flatten-candidate={f}, opaque/dispatch={o})"
    );
    let bc = BY_CLASS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = bc.as_ref() {
        let mut v: Vec<(&String, &u64)> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (k, n) in v {
            eprintln!("[CODEC-FLATTEN]     {n:>8}  {k}");
        }
    }
    eprintln!(
        "[CODEC-FLATTEN] runtime combinator-tree decode DEPTH: max={} (the flat-function collapse target)",
        MAX_DECODE_DEPTH.load(Relaxed)
    );
    let dh = DEPTH_HIST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = dh.as_ref() {
        let mut v: Vec<(&u32, &u64)> = m.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        for (d, n) in v {
            eprintln!("[CODEC-FLATTEN]     depth {d:>3}: {n:>10} decode frames");
        }
    }
}
