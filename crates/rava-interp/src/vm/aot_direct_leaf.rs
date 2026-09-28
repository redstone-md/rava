//! Direct-leaf constant resolution for `ConstDesc::AotLeaf` call sites.
//!
//! A caller compiled whole-AOT carries an `AotLeaf(owner, name, descriptor)`
//! constant where an invokestatic's direct target pointer goes. At LINK time this
//! module decides whether that symbol becomes a real leaf function pointer or 0
//! (the caller then keeps the whole-AOT compilation and its unchanged
//! `rava_invoke` fallback). The fallback contract: EVERY refusal resolves to
//! `Some(0)` — never `None`. Resolution never runs guest code: no
//! `aot_run_input`, no `pack_arguments`; it only reads the prepared archive and
//! links the leaf.

use crate::*;

/// Independent gate for the primitive leaf ABI; `RAVA_AOT_DIRECT` controls the
/// existing cached-AOT dispatch and would confound leaf-only measurements.
#[cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]
fn leaf_direct_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_AOT_DIRECT_LEAF").as_deref() == Ok("1"))
}

impl Vm {
    /// Resolve an `AotLeaf(owner, name, descriptor)` constant to the direct
    /// leaf's entry address (0 on any refusal). The linked leaf's `CompiledFn` is
    /// owned by `aot_leaf_cache` for the VM's lifetime, so the returned raw
    /// pointer stays valid for every later call through the constant table.
    #[cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]
    pub(super) fn resolve_direct_leaf(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
    ) -> Option<i64> {
        let key = (owner.to_string(), name.to_string(), descriptor.to_string());
        // Positive hits keep the fptr stable for the VM's lifetime; negative hits
        // (artifact absent) skip the archive read — the prepared build is fixed.
        if let Some(entry) = self.aot_leaf_cache.get(&key) {
            return Some(entry.as_ref().map(|f| f.code_ptr() as i64).unwrap_or(0));
        }
        match self.load_direct_leaf_artifact(owner, name, descriptor) {
            // Artifact absent / undecodable: immutable for the VM's lifetime →
            // negative-cache so later links skip the read.
            Ok(None) => {
                self.aot_leaf_cache.insert(key, None);
                Some(0)
            }
            Ok(Some(f)) => {
                let ptr = f.code_ptr() as i64;
                self.aot_leaf_cache.insert(key, Some(f));
                Some(ptr)
            }
            // Transient refusal (gate off, class not loaded/initialized, not
            // public, not a leaf): NOT cached — a later link after class init may
            // still succeed.
            Err(()) => Some(0),
        }
    }

    /// The actual direct-leaf resolution. `Ok(None)` = the prepared archive
    /// provably lacks (or cannot decode) this leaf artifact; `Err(())` = any
    /// other refusal, including the tracer gate being off.
    #[cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]
    fn load_direct_leaf_artifact(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
    ) -> Result<Option<cnp::CompiledFn>, ()> {
        // Opt-in tracer gate, and only against an explicit offline prepared build.
        if !leaf_direct_enabled() || !crate::aot_clif::prepared::enabled() {
            return Err(());
        }
        // The target class must ALREADY be loaded and fully initialized. Linking a
        // constant must not trigger class loading (get_loaded, never get_or_load)
        // and must not observe a partially initialized class — `initialized` latches
        // when init STARTS, so only `clinit_done` proves the statics are complete.
        // `clinit::settled` reads ACQUIRE, pairing with ensure_init's Release store
        // so every write the completed <clinit> made is visible before any leaf of
        // that class is linked (this link path can race another thread's class init).
        let class = self.loader.get_loaded(owner).ok_or(())?;
        if !super::clinit::settled(&class) {
            return Err(());
        }
        // Exact static target, declared by the owner itself (no inherited targets).
        let method_index = class
            .file
            .methods
            .iter()
            .position(|m| m.name == name && m.descriptor == descriptor)
            .ok_or(())?;
        let method = &class.file.methods[method_index];
        let code = method.code.as_ref().ok_or(())?;
        // ACCESS CONTROL (fail-closed): the direct call bypasses the ordinary
        // invokestatic resolution machinery, so it must not link a target a caller
        // could not invoke. Without caller-owner context, conservatively admit only
        // ACC_PUBLIC methods in ACC_PUBLIC classes; private/package-private/
        // protected targets stay on the rava_invoke fallback (resolved to 0).
        if method.access_flags & 0x0001 == 0 || class.file.access_flags & 0x0001 == 0 {
            return Err(());
        }
        // Classifier gate re-run against the live target's real metadata + body —
        // the symbolic const alone proves nothing about the target.
        if method.access_flags & 0x0008 == 0
            || !crate::aot_clif::direct_leaf::is_no_gc_primitive_leaf(
                descriptor,
                method.access_flags,
                code,
            )
        {
            return Err(());
        }
        // Same key the offline preparer wrote: same per-class identity, same
        // string-constant policy, same compiler identity (the key embeds both). The
        // runtime string-constant policy is whatever the prepared build validated,
        // not an independent env read — matching the normal link's key derivation.
        let leaf_key = crate::aot_clif::ClassCompiler::new(&class.file)
            .with_string_constants(crate::aot_clif::prepared::string_constants())
            .direct_leaf_key(method_index)
            .ok_or(())?;
        let bytes = match crate::aot_clif::prepared::read(&leaf_key) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        };
        let Some(obj) = cnp::CompiledObj::from_bytes(&bytes) else {
            return Ok(None);
        };
        // A direct leaf is ABI-bare: consts/relocs in the artifact mean it was not
        // produced by the leaf emitter — refuse rather than link foreign code.
        if !obj.consts().is_empty() || !obj.relocs().is_empty() {
            return Ok(None);
        }
        // Reloc-free code links directly; the hidden consts pointer the caller
        // passes is never dereferenced by a validated leaf (its const table is empty).
        let f = obj.link().ok_or(())?;
        Ok(Some(f))
    }

    /// Windows build: the direct arm is Linux-only; every AotLeaf constant
    /// resolves to 0 so callers keep the whole-AOT compilation + rava_invoke.
    #[cfg(all(feature = "next-gen-gc", windows, target_arch = "x86_64"))]
    pub(super) fn resolve_direct_leaf(
        &mut self,
        _owner: &str,
        _name: &str,
        _descriptor: &str,
    ) -> Option<i64> {
        Some(0)
    }
}
