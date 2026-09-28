//! Command-line configuration: the alias table, parsing, and the config file.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::*;

/// Parse a java-style argument vector. JVM options precede the jar/main class;
/// everything after the program entry point is passed through as program args.
pub(crate) fn parse(argv: &[String]) -> Result<Cli, String> {
    let mut classpath = Vec::new();
    let mut props = HashMap::new();
    let mut memory_mb = None;
    let sep = if cfg!(windows) { ';' } else { ':' };

    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        match a.as_str() {
            "-jar" => {
                let path = argv.get(i + 1).ok_or("-jar requires a file")?.clone();
                let args = argv.get(i + 2..).unwrap_or(&[]).to_vec();
                return Ok(Cli { launch: Launch::Jar(path, args), classpath, props, memory_mb });
            }
            "-cp" | "-classpath" | "--class-path" => {
                let p = argv.get(i + 1).ok_or("-cp requires a path")?;
                classpath.extend(p.split(sep).filter(|s| !s.is_empty()).map(PathBuf::from));
                i += 2;
            }
            // Options that take a following value we don't use — skip both.
            "-p" | "--module-path" | "--add-opens" | "--add-exports" | "--add-modules"
            | "-m" | "--module" | "--patch-module" | "--upgrade-module-path" => { i += 2; }
            _ if a.starts_with("-Xmx") => { memory_mb = parse_size_mb(&a[4..]); i += 1; }
            _ if a.starts_with("-Xms") => { i += 1; } // initial heap — not modelled
            _ if a.starts_with("-D") => {
                if let Some((k, v)) = a[2..].split_once('=') {
                    props.insert(k.to_string(), v.to_string());
                }
                i += 1;
            }
            // Any other flag (-X…, -XX:…, -ea, -verbose…, -server, --enable-preview…): ignore.
            _ if a.starts_with('-') => { i += 1; }
            // First bare token is the main class; the rest are program args.
            _ => {
                let name = a.replace('.', "/");
                let args = argv.get(i + 1..).unwrap_or(&[]).to_vec();
                return Ok(Cli { launch: Launch::Class(name, args), classpath, props, memory_mb });
            }
        }
    }
    Ok(Cli { launch: Launch::None, classpath, props, memory_mb })
}

/// Runtime tunables, persisted to `rava.toml`. `threads` records the detected
/// core count for the (future) multi-threaded executor; it is not yet wired to
/// parallel execution — the interpreter still runs the program on one thread.
pub(crate) struct Config {
    pub(crate) memory_mb: u64,
    pub(crate) threads: usize,
    pub(crate) jit: bool,
    pub(crate) jit_cap: usize,
    /// Explicit compile threshold from shell env / `rava.toml`, if set anywhere. `None` defers
    /// to the engine's tier-aware default (16 under tier-0, otherwise 512): publishing a
    /// CLI-side value here would shadow that fallback on the CLI path.
    pub(crate) jit_threshold: Option<u32>,
    pub(crate) gc: bool,
}

impl Config {
    const DEFAULT_MEMORY_MB: u64 = 8192;
    // The shared JIT module removed the old per-method mmap/RSS reason for a 4,096-method cap.
    // Keeping that stale default leaves the hot long tail interpreted and costs roughly a third
    // of startup wall in the fixed-work boot gate. 16,384 stays within the documented ~40 MiB
    // code budget and is also the interpreter's fallback when the CLI does not publish a cap.
    const DEFAULT_JIT_CAP: usize = 16_384;

    /// Resolve each tunable by precedence: environment → `rava.toml` → default
    /// (memory also takes the CLI `-Xmx` first). JIT and GC default ON. The JIT threshold
    /// is the exception: absent stays `None` and defers to the engine's tier-aware default
    /// (16 under tier-0, otherwise 512) — the CLI holds no default of its own for it.
    pub(crate) fn resolve(cli_mem: Option<u64>) -> Config {
        let file = parse_config_file();
        // Export every raw `RAVA_*` key from rava.toml into the process env FIRST (before the
        // friendly-alias resolution below and before main() reads RAVA_HARD_MB / RAVA_PROC_CAP_MB).
        // This is the single-config-file mechanism: the engine reads ~226 tunables via
        // std::env::var scattered across the crate, so setting them here from the file lets ONE
        // rava.toml drive all of them — with the shell env / CLI still overriding (only-if-unset).
        apply_raw_flags(&file);
        // Then resolve the CLEAN sectioned names ([jit] threshold = …) onto their legacy
        // RAVA_* envs — the human-readable layer over the same only-if-unset rule.
        apply_clean_aliases(&file);
        let detected = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let threads = match file.get("threads").and_then(|v| v.parse::<usize>().ok()) {
            Some(0) | None => detected,
            Some(n) => n,
        };
        let memory_mb = cli_mem
            .or_else(|| std::env::var("RAVA_XMX_MB").ok().and_then(|v| v.trim().parse().ok()))
            .or_else(|| file.get("memory_mb").and_then(|v| v.parse().ok()))
            .unwrap_or(Self::DEFAULT_MEMORY_MB);
        let jit = std::env::var("RAVA_JIT").ok().map(|v| cfg_bool(&v))
            .or_else(|| file.get("jit").map(|v| cfg_bool(v)))
            .unwrap_or(true);
        let jit_cap = std::env::var("RAVA_JIT_CAP").ok().and_then(|v| v.trim().parse().ok())
            .or_else(|| file.get("jit_cap").and_then(|v| v.parse().ok()))
            .unwrap_or(Self::DEFAULT_JIT_CAP);
        // No CLI-side default (was `.unwrap_or(512)`): absent stays `None`, so the engine's
        // tier-aware fallback (16 under tier-0, otherwise 512) is the single owner of the
        // default. `RAVA_JIT_THRESHOLD` here already folds in the raw and section aliases
        // exported above only-if-unset, so shell env still wins over the file.
        let jit_threshold = std::env::var("RAVA_JIT_THRESHOLD").ok().and_then(|v| v.trim().parse().ok())
            .or_else(|| file.get("jit_threshold").and_then(|v| v.parse().ok()));
        let gc = std::env::var("RAVA_GC").ok().map(|v| cfg_bool(&v))
            .or_else(|| file.get("gc").map(|v| cfg_bool(v)))
            .unwrap_or(true);
        let cfg = Config { memory_mb, threads, jit, jit_cap, jit_threshold, gc };
        if file.is_empty() { cfg.write_default(); }
        cfg
    }

    /// Publish the resolved values as `RAVA_*` env vars the engine reads. The threshold is
    /// exported only when explicit — an absent one must stay unset so the engine's
    /// tier-aware default (16 under tier-0, otherwise 512) decides.
    pub(crate) fn apply(&self) {
        std::env::set_var("RAVA_XMX_MB", self.memory_mb.to_string());
        std::env::set_var("RAVA_THREADS", self.threads.to_string());
        std::env::set_var("RAVA_JIT", if self.jit { "true" } else { "false" });
        std::env::set_var("RAVA_JIT_CAP", self.jit_cap.to_string());
        if let Some(threshold) = self.jit_threshold {
            std::env::set_var("RAVA_JIT_THRESHOLD", threshold.to_string());
        }
        std::env::set_var("RAVA_GC", if self.gc { "true" } else { "false" });
        if std::env::var("RAVA_VERBOSE").is_ok() {
            // `auto` = no explicit threshold anywhere; the engine then picks
            // 16 under tier-0, otherwise 512.
            let threshold = match self.jit_threshold {
                Some(t) => t.to_string(),
                None => "auto".to_string(),
            };
            eprintln!(
                "rava: memory cap {} MiB, {} core(s), jit={} (cap {}, threshold {}), gc={}",
                self.memory_mb, self.threads, self.jit, self.jit_cap, threshold, self.gc
            );
        }
    }

    pub(crate) fn write_default(&self) {
        // Self-documenting default in the CLEAN sectioned style (see CONFIG_ALIASES). Every raw
        // RAVA_* tunable can still be set at top level verbatim; CLI/-Xmx and shell env override.
        // The threshold ships commented out unless one was resolved explicitly (possible even
        // with no file present — shell env alone): an active value here would pin the knob and
        // shadow the engine's tier-aware default (16 under tier-0, otherwise 512).
        let threshold = match self.jit_threshold {
            Some(t) => format!("threshold = {t}           # invocations before a method compiles\n"),
            None => "# threshold = 512           # invocations before a method compiles; unset defers to the\n\
                     #                           engine default (16 under tier-0, otherwise 512)\n"
                .to_string(),
        };
        let body = format!(
            "# ═════════════════════════════════════════════════════════════════════════\n\
             #  RAVA — runtime configuration\n\
             #  Precedence:  CLI / -Xmx  >  shell env  >  this file  >  built-in default.\n\
             #  Sectioned keys are the clean names; raw `RAVA_* = 1` keys at top level\n\
             #  also work (escape hatch for deep forensics switches).\n\
             # ═════════════════════════════════════════════════════════════════════════\n\
             \n\
             threads = {}                # worker cores; 0 = auto-detect\n\
             \n\
             [memory]\n\
             limit_mb = {}            # soft heap cap (like -Xmx)\n\
             # os_cap_mb = 4096         # kernel-enforced process cap (Windows Job Object)\n\
             # hard_cap_mb = 3584       # allocator-level real-memory ceiling\n\
             # max_stack_depth = 20000  # guest recursion limit\n\
             \n\
             [jit]\n\
             enabled = {}             # compile hot methods to native code (Cranelift)\n\
             max_methods = {}         # compiled-method cap (bounds JIT code memory)\n\
            {}\
             tier0 = true               # tier-0 partial-inline regime\n\
             # flat_getfield = true     # method-entry arena unswitch (de-diamonds one receiver)\n\
             # dump = \"ClassName\"     # dump CLIF + machine code for matching methods\n\
             \n\
             [natives]\n\
             gson = true                # native JSON parse path\n\
             bitstorage = true          # SimpleBitStorage native kernel\n\
             \n\
             [gc]\n\
             enabled = {}\n\
             \n\
             [diagnostics]\n\
             # verbose = true           # print resolved config at startup\n\
             # census = true            # heap breakdown at each GC peak\n\
             # wall_profile = true      # 10ms wall-time phase sampler (+ top-20 methods)\n\
             # wall_profile_every_ms = 30000  # dump period; deltas separate guest phases\n\
             # pc_profile = true        # RIP sampling: sees through PIC-resolved subtrees\n\
             # pc_stacks = true         # + caller attribution for each RIP sample\n\
             # pc_stack_filter = \"memchr\"  # attribute only samples whose leaf matches\n\
             # bridge_stats = true      # native-bridge time tally (slows hot paths)\n",
            self.threads, self.memory_mb, self.jit, self.jit_cap, threshold, self.gc
        );
        let _ = std::fs::write("rava.toml", body);
    }
}

/// Read `rava.toml` from the working directory into a flat key→value map. A
/// hand-rolled parser (no toml dependency): `key = value`, `#` comments.
/// `[section]` headers PREFIX the keys inside them (`[jit] threshold` → `jit.threshold`),
/// which is what the human-readable alias layer below resolves; a bare key (no section)
/// keeps its plain name, so legacy top-level `RAVA_*` keys keep working verbatim.
pub(crate) fn parse_config_file() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let text = match std::fs::read_to_string("rava.toml") { Ok(t) => t, Err(_) => return map };
    let mut section = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = if section.is_empty() || k.trim().starts_with("RAVA_") {
                k.trim().to_string()
            } else {
                format!("{section}.{}", k.trim())
            };
            map.insert(key, v.trim().trim_matches('"').to_string());
        }
    }
    map
}

pub(crate) const CONFIG_ALIASES: &[(&str, &str, AliasKind)] = &[
    // [memory]
    ("memory.limit_mb", "RAVA_XMX_MB", AliasKind::Val),
    ("memory.os_cap_mb", "RAVA_PROC_CAP_MB", AliasKind::Val),
    ("memory.hard_cap_mb", "RAVA_HARD_MB", AliasKind::Val),
    ("memory.nursery_kb", "RAVA_NURSERY_KB", AliasKind::Val),
    ("memory.gc_major_kb", "RAVA_ARENA_MAJOR_KB", AliasKind::Val),
    ("memory.max_stack_depth", "RAVA_MAX_DEPTH", AliasKind::Val),
    ("memory.arena_decommit", "RAVA_ARENA_DECOMMIT", AliasKind::Flag),
    ("memory.array_trim", "RAVA_ARRAY_TRIM", AliasKind::Flag),
    ("memory.lambda_dedup", "RAVA_M3_LAMBDA_DEDUP", AliasKind::Val),
    ("memory.gc_rearm_tight", "RAVA_GC_REARM_TIGHT", AliasKind::Val),
    // [jit]
    ("jit.enabled", "RAVA_JIT", AliasKind::Val),
    ("jit.max_methods", "RAVA_JIT_CAP", AliasKind::Val),
    ("jit.threshold", "RAVA_JIT_THRESHOLD", AliasKind::Val),
    ("jit.tier0", "RAVA_JIT_TIER0", AliasKind::Flag),
    ("jit.inline_max_bytecode", "RAVA_INLINE_MAX_BYTECODE", AliasKind::Val),
    ("jit.deny", "RAVA_JIT_DENY", AliasKind::Val),
    ("jit.dump", "RAVA_JIT_DUMP", AliasKind::Val),
    ("jit.verify", "RAVA_JIT_VERIFY", AliasKind::Flag),
    ("jit.pic_ways", "RAVA_PIC_CAP", AliasKind::Val),
    ("jit.ic_deep", "RAVA_NO_IC_DEEP", AliasKind::Inv),
    ("jit.callbuf_wide", "RAVA_NO_CALLBUF_WIDE", AliasKind::Inv),
    ("parallel.workers", "RAVA_EXEC_THREADS", AliasKind::Val),
    // Synthetic-layout classes are arena-born rather than handle-born (opt-in; row 436 flip
    // withdrawn in row 437 — the disjoint pair that justified it did not survive six boots).
    ("gc.arena_synth", "RAVA_ARENA_SYNTH", AliasKind::Flag),
    // Async bodies run on the task runtime instead of the calling thread (default-ON, row 427).
    ("parallel.async_offload", "RAVA_NO_ASYNC_OFFLOAD", AliasKind::Inv),
    ("parallel.ring_lane", "RAVA_NO_GILFREE_RINGS", AliasKind::Inv),
    ("parallel.array_owned", "RAVA_NO_GILFREE_ARRAY_OWNED", AliasKind::Inv),
    ("parallel.warm_compile", "RAVA_NO_LANE_WARM_COMPILE", AliasKind::Inv),
    ("parallel.freeze_resolve", "RAVA_NO_M3_FREEZE_RESOLVE", AliasKind::Inv),
    ("parallel.ws_set_contains", "RAVA_NO_WS_SET_CONTAINS", AliasKind::Inv),
    ("jit.wide_pic", "RAVA_NO_WIDE_PIC", AliasKind::Inv),
    ("jit.heap_pic", "RAVA_NO_JIT_HEAPPIC", AliasKind::Inv),
    ("jit.lambda_pic", "RAVA_LAMBDA_PIC", AliasKind::Flag),
    ("jit.lambda_impl_key", "RAVA_LAMBDA_IMPL_KEY", AliasKind::Flag),
    ("jit.direct_mono", "RAVA_NO_JIT_DIRECTMONO", AliasKind::Inv),
    // (jit.dispatch_fastframe / RAVA_DISPATCH_FASTFRAME retired 2026-07-27 - the warm
    // direct call takes its thread context once, unconditionally. See jit_direct_invoke.)
    ("jit.loop_bce", "RAVA_NO_LOOP_BCE", AliasKind::Inv),
    ("jit.tiny_inline", "RAVA_NO_TINY", AliasKind::Inv),
    ("jit.field_promotion", "RAVA_NO_FIELD_PROMO", AliasKind::Inv),
    ("jit.capturing_lambda_direct", "RAVA_NO_CAP_LAMBDA", AliasKind::Inv),
    ("jit.parallel_kernels", "RAVA_NO_JIT_PARALLEL", AliasKind::Inv),
    ("jit.inline_boxing", "RAVA_NO_IL_BOXING", AliasKind::Inv),
    ("jit.inline_getters", "RAVA_NO_IL_GET", AliasKind::Inv),
    ("jit.arena_inline", "RAVA_NO_ARENA_INLINE", AliasKind::Inv),
    ("jit.flat_getfield", "RAVA_FLAT_GETFIELD", AliasKind::Flag),
    // Extend the gen-guarded getstatic value cells to REFERENCE statics (unstable
    // BOX packs skipped at refill; see jit::ref_static_cells_enabled). Default OFF.
    ("jit.ref_static_cells", "RAVA_NO_REF_STATIC_CELLS", AliasKind::Inv), // DEFAULT-ON 2026-07-26: Done window -30%
    // Identity-keyed memo for provably effect-free calls (see crates/rava-interp/src/pure_memo.rs).
    ("jit.pure_memo", "RAVA_PURE_MEMO", AliasKind::Flag),
    ("jit.pure_memo_probe", "RAVA_PURE_MEMO_PROBE", AliasKind::Flag),
    ("jit.bitset_inline", "RAVA_NO_BITSET_INLINE", AliasKind::Inv),
    // Fold byte-slice (class-name / member-name) FastMap keys a word at a time instead of a
    // byte at a time. Engine-internal hashing only — no guest-visible semantics. DEFAULT-ON.
    ("jit.fast_strhash", "RAVA_NO_FAST_STRHASH", AliasKind::Inv),
    // Serve the (owner, name) -> field-id memo from a reused scratch key instead of
    // allocating two Strings and SipHashing them on every hit. DEFAULT-ON.
    ("jit.fieldkey_scratch", "RAVA_NO_FIELDKEY_SCRATCH", AliasKind::Inv),
    // Serve the fastutil scalar fast path's (`a`, `size`) field identities from a
    // per-class memo instead of re-resolving them BY NAME on every call. The chain
    // `resolve_field_id < fu_double_view < jit_fu_invoke` is 6.7% of the ring window
    // and the only chain under `resolve_field_id` in it. DEFAULT-OFF.
    ("jit.fu_field_ids", "RAVA_NO_FU_FIELD_IDS", AliasKind::Inv), // DEFAULT-ON 2026-07-27: Done -9.11%
    // Let a gil-free worker serve Set.contains from an already-synced membership index
    // instead of falling through to the one global resolve lock. DEFAULT-OFF.
    ("jit.fu_field_ids_probe", "RAVA_FU_FIELD_IDS_PROBE", AliasKind::Flag),
    // Build a lambda apply's argument vector by BORROWING the closure's captures and
    // splicing once, instead of cloning the whole `Lambda`, cloning its captures again
    // and then growing that clone to append the SAM args. DEFAULT-OFF pending its A/B.
    ("jit.lambda_splice", "RAVA_LAMBDA_SPLICE", AliasKind::Flag),
    // Census of lambda applies (total / non-capturing / captured values). Counts only.
    ("jit.lambda_stat", "RAVA_LAMBDA_STAT", AliasKind::Flag),
    // Reach the per-thread cache clusters (tlcache / frames / owner_bits) through an
    // inlined TLS cursor instead of a `LocalKey::with` call per access. `LocalKey::with`
    // is 5.5-8.9% of RIP self-time in EVERY window. DEFAULT-OFF.
    //
    // THE ONE FLAG ON THIS AXIS. A crossing census (RAVA_TLS_STAT) of a real boot shows
    // this covers the whole population - 1.654G crossings, all three clusters, every
    // window - where the retired fast-frame flag covered 38.4% of it and only inside
    // `frames` (13.0% of blocks A+B, 1.0% of block C, 55.0% of the Done window). Anything
    // else claiming to remove `LocalKey::with` must be priced against that census, and
    // its number SUBTRACTED from this one over the overlap, never added.
    ("jit.tls_cluster", "RAVA_TLS_CLUSTER", AliasKind::Flag),
    // Read the process-constant forensic gates on the compiled->host callback path ONCE
    // instead of issuing a `RtlQueryEnvironmentVariable` per call.
    ("jit.env_memo", "RAVA_NO_ENV_MEMO", AliasKind::Inv), // DEFAULT-ON 2026-07-26
    // Answer the dispatch gauntlet's substring questions about an owner class name from a
    // per-name memo instead of re-scanning the name once per question.
    ("natives.owner_bits", "RAVA_NO_OWNER_BITS", AliasKind::Inv), // DEFAULT-ON 2026-07-26
    // Skip the whole native-dispatch gauntlet for an owner no arm of it keys on. The
    // cascade runs once per SUPER-CHAIN ENTRY, so a jar class pays it at every level to
    // conclude "not a native"; the owner alone decides that. DEFAULT-ON.
    ("natives.dispatch_gate", "RAVA_DISPATCH_GATE", AliasKind::Flag), // DEFAULT-OFF pending A/B
    // Run the gauntlet even for a gate-rejected owner and report any arm that would
    // have served it — the drift check for the gate's superset claim. DEFAULT-OFF.
    ("natives.dispatch_gate_verify", "RAVA_DISPATCH_GATE_VERIFY", AliasKind::Flag),
    // Build the anewarray `L<name>;` element descriptor in a stack buffer instead of two
    // heap Strings per allocation (jit_anewarray = 2.3-2.9% of RIP samples). DEFAULT-OFF.
    ("jit.anewarray_scratch", "RAVA_ANEWARRAY_SCRATCH", AliasKind::Flag),
    // Return the pooled argument buffer to the thread-local pool on the NATIVE dispatch
    // arms, which only BORROW it and used to drop it. Measured over blocks A+B: the pool
    // leaked 21.5M buffers and missed 43% of takes, so `pop_args` handed back `Vec::new()`
    // and the caller re-grew it — the `finish_grow` behind the allocator cluster (11.7% of
    // A+B). Recycling takes the miss rate to 6.0% and Vec regrows 21.7M -> 4.2M. DEFAULT-OFF.
    ("interp.argbuf_recycle", "RAVA_ARGBUF_RECYCLE", AliasKind::Flag),
    // Serve `instanceof` from a per-(site, receiver class) memo. The codegen emits no
    // inline type test, so every compiled 0xc1 is a host callback that re-runs a
    // String-cloning hierarchy walk whose answer only depends on (target, class).
    // DEFAULT-OFF.
    ("jit.instanceof_memo", "RAVA_NO_INSTANCEOF_MEMO", AliasKind::Inv),
    // Share one Arc<str> per receiver class for invokevirtual's dispatch class name. The
    // invoke fallback's top rows are native-served collection methods that never populate
    // the per-(site,class) dispatch caches, so each one rebuilt that name - a String alloc,
    // and for an arena receiver a global class-registry mutex too. DEFAULT-OFF.
    ("jit.virt_class_memo", "RAVA_VIRT_CLASS_MEMO", AliasKind::Flag),
    // [natives]
    // Bridge-prologue arg canonicalization from a pooled buffer with one copy per string
    // argument instead of a fresh Vec + two copies per string. DEFAULT-OFF.
    ("natives.canon_scratch", "RAVA_CANON_SCRATCH", AliasKind::Flag),
    ("natives.gson", "RAVA_GSON_NATIVE", AliasKind::Flag),
    // Extract a composite comparator's clause key on FIRST TOUCH instead of building every
    // element's whole tuple up front, restoring `thenComparing` short-circuiting to the
    // cached path. DEFAULT-OFF.
    ("natives.sort_key_memo", "RAVA_SORT_KEY_MEMO", AliasKind::Flag),
    ("natives.bitstorage", "RAVA_M3_BITSTORAGE", AliasKind::Flag),
    ("natives.spline", "RAVA_NO_SPLINE_NATIVE", AliasKind::Inv),
    ("natives.registry_map", "RAVA_NO_R2OMAP", AliasKind::Inv),
    ("natives.map_combo_hash", "RAVA_NO_MAP_COMBO_HASH", AliasKind::Inv),
    ("natives.map_key_memo", "RAVA_NO_KEYMEMO", AliasKind::Inv),
    ("natives.arena_maps", "RAVA_ARENA_MAPS", AliasKind::Flag),
    ("natives.datapack_parallel", "RAVA_M4_DATAPACK_PARALLEL", AliasKind::Flag),
    ("natives.mapkey_hashcode", "RAVA_MAPKEY_HASHCODE", AliasKind::Flag),
    ("natives.record_tostring_pure", "RAVA_RECORD_TOSTRING_PURE", AliasKind::Flag),
    // Real JVM ObjectMethods semantics for a record's synthesised hashCode/equals:
    // recursive component hash/compare (wrapper hash rules, virtual dispatch for refs)
    // instead of hashing/comparing toString renders. Default OFF: changes guest-visible
    // record hash VALUES (to the JVM's own deterministic ones).
    ("natives.record_semantics", "RAVA_RECORD_SEMANTICS", AliasKind::Flag),
    // Hash-cons value-object map keys. DEFAULT-ON since 2026-07-26 (RandomState.create −81%).
    ("maps.intern_keys", "RAVA_NO_KEY_INTERN", AliasKind::Inv),
    // Probe the key-intern token memo BEFORE the per-render class-name / purity-proof /
    // resolve_method prologue instead of after it. The memo already made the derivation
    // O(distinct nodes) of rendering; this makes the BOOKKEEPING O(distinct nodes) too.
    ("maps.token_probe_first", "RAVA_TOKEN_PROBE_FIRST", AliasKind::Flag),
    // Resolve a record call site's class name + component field ids ONCE per
    // (site, receiver class) instead of on every synthesised toString/hashCode/equals.
    ("natives.record_link", "RAVA_RECORD_LINK", AliasKind::Flag),
    // [gc]
    // Value-model step 1 (box-arm retirement): strings in arena ref cells born 0b00.
    ("arena.str_refcell", "RAVA_NO_ARENA_STR_REFCELL", AliasKind::Inv),
    ("gc.enabled", "RAVA_GC", AliasKind::Val),
    ("gc.validate", "RAVA_GC_VALIDATE", AliasKind::Flag),
    ("gc.poison", "RAVA_GC_POISON", AliasKind::Flag),
    ("gc.shadow", "RAVA_SHADOW_GC", AliasKind::Flag),
    ("gc.receiver_guard", "RAVA_RECEIVER_GUARD", AliasKind::Flag),
    // Default-ON since 2026-08-07 (RSS −11.1% at neutral wall, ledger row 105), so the
    // config knob is the OPT-OUT — `Inv` maps `false` in the file to setting RAVA_NO_*.
    ("gc.birth_ns_guard", "RAVA_NO_ARENA_BIRTH_NS_GUARD", AliasKind::Inv),
    ("gc.arena_lambda", "RAVA_NO_ARENA_LAMBDA", AliasKind::Inv),
    ("gc.arena_lambda_dedup", "RAVA_NO_ARENA_LAMBDA_DEDUP", AliasKind::Inv),
    ("gc.arena_lambda_singleton", "RAVA_NO_ARENA_LAMBDA_SINGLETON", AliasKind::Inv),
    // Per-`new`/`anewarray`-site memo of the allocation ROUTE (handle/arena/…):
    // skips the per-alloc class-name String + name-keyed lookups + collection
    // substring scans. Decision inputs are process-permanent. Default OFF.
    // Default-ON since 2026-08-07 (ledger row 108), so the knob is the OPT-OUT.
    ("gc.alloc_route_memo", "RAVA_NO_ALLOC_ROUTE_MEMO", AliasKind::Inv),
    ("gc.stats", "RAVA_GC_STATS", AliasKind::Flag),
    // [multicore] — the parallel worldgen fork (experimental; see docs + memory)
    ("multicore.fork_workers", "RAVA_FORK_WORKERS", AliasKind::Val),
    ("multicore.ic_parallel", "RAVA_M3_IC_PARALLEL", AliasKind::Flag),
    ("multicore.worker_locked_exec", "RAVA_M3_WORKER_LOCKED_EXEC", AliasKind::Flag),
    ("multicore.compiled_body", "RAVA_M3_S2", AliasKind::Flag),
    ("multicore.chunk_fill_ready", "RAVA_M3_CHUNK_FILL_READY", AliasKind::Flag),
    ("multicore.chunk_fill_parallel", "RAVA_M3_CHUNK_FILL_PARALLEL", AliasKind::Flag),
    ("multicore.fork_cow", "RAVA_NO_FORK_COW", AliasKind::Inv),
    ("multicore.cow_owned_bypass", "RAVA_COW_OWNED_BYPASS", AliasKind::Flag),
    ("multicore.cow_owned_undo", "RAVA_COW_OWNED_UNDO", AliasKind::Flag),
    // (multicore.fork_column / multicore.fork_min_elements were RETIRED with the
    // RAVA_FORK_COLUMN prototype at GAP-S09 stage 5 — superseded by the S2 fork.)
    // [maps] GAP-P04: equals-verified salted value-object map keys. Default OFF.
    // Real-boot A/B: peak WS −466 MiB (37.7%) repeatable, wall mixed (+2.6% median);
    // canonical ChunkFill: bit-exact but +145 MiB (GC pacing) — see the gap register.
    ("maps.salt_keys", "RAVA_MAPKEY_SALT", AliasKind::Flag),
    ("multicore.os_threads", "RAVA_OS_THREAD", AliasKind::Flag),
    // [aot]
    ("aot.clif_cache", "RAVA_AOT_CLIF", AliasKind::Flag),
    ("aot.dir", "RAVA_AOT_DIR", AliasKind::Val),
    ("aot.llvm", "RAVA_LLVM_AOT", AliasKind::Flag),
    ("aot.llvm_march", "RAVA_LLVM_MARCH", AliasKind::Val),
    ("aot.clang", "RAVA_CLANG", AliasKind::Val),
    // [diagnostics]
    ("diagnostics.verbose", "RAVA_VERBOSE", AliasKind::Flag),
    ("diagnostics.census", "RAVA_CENSUS", AliasKind::Flag),
    ("diagnostics.gc_rearm_trace", "RAVA_GC_REARM_TRACE", AliasKind::Flag),
    ("diagnostics.gil_stats", "RAVA_GILSTAT", AliasKind::Flag),
    ("diagnostics.bridge_stats", "RAVA_BRIDGE_STAT", AliasKind::Flag),
    ("diagnostics.map_key_stats", "RAVA_KEYSTAT", AliasKind::Flag),
    // Per-cluster `LocalKey::with` crossing census (frames / tlcache / owner_bits), the
    // contention-immune way to price tls_cluster and anything claiming the same removal.
    // Counted only on the cluster's disabled arm, so it never touches a shipped fast path.
    ("diagnostics.tls_stats", "RAVA_TLS_STAT", AliasKind::Flag),
    ("diagnostics.jit_stats", "RAVA_JIT_STATS", AliasKind::Flag),
    ("diagnostics.wall_profile", "RAVA_PROFILE_WALL", AliasKind::Flag),
    ("diagnostics.wall_profile_every_ms", "RAVA_PROFILE_WALL_EVERY_MS", AliasKind::Val),
    // PC sampling: decomposes JIT_EXEC by the guest thread's actual instruction pointer,
    // which sees through PIC-resolved subtrees the entry-self-time table collapses into
    // one row. Implies wall_profile. Windows-only; no-op elsewhere.
    ("diagnostics.pc_profile", "RAVA_PROFILE_PC", AliasKind::Flag),
    // Caller attribution for the above: copies the top of the guest stack inside the same
    // suspend window and recovers candidate return addresses, which is what turns a leaf
    // symbol (`memchr`) into a removable cost (the engine routine that called it).
    // Implies pc_profile. Windows-only.
    ("diagnostics.pc_stacks", "RAVA_PROFILE_PC_STACKS", AliasKind::Flag),
    // Comma-separated substrings; only samples whose LEAF symbol matches are attributed.
    ("diagnostics.pc_stack_filter", "RAVA_PC_STACK_FILTER", AliasKind::Val),
    ("diagnostics.profiler", "RAVA_PROF", AliasKind::Flag),
    ("diagnostics.profiler_tree", "RAVA_PROF_TREE", AliasKind::Flag),
    ("diagnostics.heartbeat", "RAVA_HEARTBEAT", AliasKind::Flag),
    ("diagnostics.alloc_histogram", "RAVA_ALLOC_HISTO", AliasKind::Flag),
    ("diagnostics.fork_trace", "RAVA_FORK_TRACE", AliasKind::Flag),
    ("diagnostics.fork_stats", "RAVA_FORK_STATS", AliasKind::Flag),
    ("diagnostics.fork_gate_debug", "RAVA_DBG_PAR", AliasKind::Flag),
    ("diagnostics.valueof_probe", "RAVA_VALUEOF_PROBE", AliasKind::Flag),
    ("diagnostics.residue_probe", "RAVA_RESIDUE_PROBE", AliasKind::Flag),
    // Compiled→interpreter fallback tally: which (site, opcode) fall out of
    // compiled code into interpreter handlers, and why (see fallback_tally.rs).
    ("diagnostics.fallback_tally", "RAVA_FALLBACK_TALLY", AliasKind::Flag),
    ("diagnostics.tick_profile", "RAVA_TICK_PROF", AliasKind::Flag),
    ("diagnostics.compile_time", "RAVA_COMPILE_TIME", AliasKind::Flag),
];

/// Resolve the clean sectioned aliases into their legacy `RAVA_*` env vars (only-if-unset,
/// so shell env / CLI still wins — same precedence rule as `apply_raw_flags`).
pub(crate) fn apply_clean_aliases(file: &HashMap<String, String>) {
    for (clean, legacy, kind) in CONFIG_ALIASES {
        let Some(v) = file.get(*clean) else { continue };
        if std::env::var_os(legacy).is_some() {
            continue; // real env wins
        }
        match kind {
            AliasKind::Val => {
                if !v.is_empty() {
                    std::env::set_var(legacy, v);
                }
            }
            AliasKind::Flag => {
                if truthy(v) {
                    std::env::set_var(legacy, "1");
                }
            }
            AliasKind::Inv => {
                if !truthy(v) {
                    std::env::set_var(legacy, "1"); // the RAVA_NO_* kill-switch
                }
            }
        }
    }
}

// --- launchers --------------------------------------------------------------
