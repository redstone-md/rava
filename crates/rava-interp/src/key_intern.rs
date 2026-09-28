//! Hash-consing for value-equality map keys (default ON; `RAVA_NO_KEY_INTERN` disables).
//!
//! A value object with no synthetic name field is keyed by its `toString()` tree
//! (`map::key_str_mut_in`). That derivation flattens a DAG: a node shared by `k`
//! parents contributes its whole subtree text `k` times, and every level copies
//! its children's text again. Measured on the worldgen density graph, one key
//! reaches 98 KB (651 KB at the extreme) and a single `RandomState.create` builds
//! ~21 MB of key text — which is why the derivation costs O(flattened text)
//! rather than O(distinct nodes).
//!
//! While a key derivation is in progress, every NESTED render is replaced by a
//! fixed-width token naming the interned text it produced. Injectivity is exact
//! and inductive: two nodes share a token iff their token-form texts are equal
//! iff (by induction on the render tree) their full texts are equal. So the
//! scheme never MERGES two keys the string form kept apart — it can only split
//! two objects whose full texts coincided through differently shaped renders.
//!
//! Substitution is admitted only inside a chain of renders that are ALL proven
//! pure, so a token can never be published to guest-visible state: an impure
//! `toString` anywhere on the stack renders its whole subtree the historic way.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Token marker — a C0 control character, which no real `toString` emits.
const MARK: char = '\u{1}';

/// Renders shorter than this keep their text: the interning table is global and
/// locked, so paying for it on a 30-byte `Identifier` costs more than the copy it
/// saves. Injectivity is untouched — equal texts have equal length, so both sides
/// of any comparison take the same branch — and the collapse still bounds the
/// growth, because a parent whose children stayed inline crosses the threshold
/// itself within a level or two.
const MIN_INTERN_BYTES: usize = 48;

/// Whether a render of this size is worth naming by a token.
pub(crate) fn worth_interning(rendered: &str) -> bool {
    rendered.len() >= MIN_INTERN_BYTES
}

thread_local! {
    /// Nesting depth of an in-progress map-key derivation.
    static KEY_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Nesting depth of object renders inside `Vm::value_to_string`.
    static RENDER_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// True while EVERY enclosing render frame was proven pure by its own body.
    static CHAIN_PURE: Cell<bool> = const { Cell::new(true) };
}

/// Token table. `crate::FastMap`, NOT the std default hasher: the growth-ranked lane profile
/// (4 vs 6 workers) puts `core::hash::BuildHasher::hash_one<RandomState, &str>` at 0% → 2.90%,
/// i.e. SipHash over string keys is one of the rows that only exists at the wider width, and this
/// table is consulted per key TOKEN from every worker in the ring. Swapping the hasher is
/// behaviour-preserving here because ids come from `table.len()` at INSERTION time - insertion
/// order, never hash order - and the table is never iterated.
static TABLE: OnceLock<Mutex<crate::FastMap<Box<str>, u32>>> = OnceLock::new();

/// DEFAULT-ON since 2026-07-26 (`RAVA_NO_KEY_INTERN` disables).
///
/// Flipped on three agreeing measurements: the isolated probe put `Bootstrap.bootStrap` at
/// 58783 → 51152 ms (−13.0%) and `RandomState.create` at −81%, and two independently ordered
/// interleaved block-A+B boot batches then returned min −13.4% / median −10.9% — the wall
/// figure matching the isolated one almost exactly, so mechanism and magnitude agree rather
/// than merely pointing the same way.
///
/// Safe to default because the transform can only ever SPLIT keys, never MERGE them:
/// injectivity is inductive and exact, and merging is the only failure mode that would
/// corrupt a registry. ChunkFill `8790f04ace726ecf` and RingScan `37539c6c375d0c3` hold on
/// both arms.
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_KEY_INTERN").is_none())
}

/// RAII: a map-key derivation is in progress for the lifetime of this guard.
/// Nested derivations (a `toString` that itself performs a map lookup) share the
/// outermost scope — the token namespace is process-global, so that is coherent.
pub(crate) struct KeyScope(bool);

impl KeyScope {
    pub(crate) fn enter() -> Self {
        if !enabled() {
            return Self(false);
        }
        KEY_DEPTH.with(|d| d.set(d.get() + 1));
        Self(true)
    }
}

impl Drop for KeyScope {
    fn drop(&mut self) {
        if self.0 {
            KEY_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
}

/// RAII around one object render: tracks nesting depth and carries the
/// all-enclosing-frames-are-pure invariant down the subtree.
pub(crate) struct RenderGuard(bool, bool);

impl RenderGuard {
    pub(crate) fn enter(body_pure: bool) -> Self {
        if !enabled() {
            return Self(false, true);
        }
        RENDER_DEPTH.with(|d| d.set(d.get() + 1));
        let previous = CHAIN_PURE.with(Cell::get);
        CHAIN_PURE.with(|c| c.set(previous && body_pure));
        Self(true, previous)
    }
}

impl Drop for RenderGuard {
    fn drop(&mut self) {
        if self.0 {
            RENDER_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            CHAIN_PURE.with(|c| c.set(self.1));
        }
    }
}

/// The CONTEXT half of `should_substitute`: everything that depends on WHERE the
/// render sits rather than on WHAT is being rendered. Three thread-local `Cell`
/// reads, no hashing and no allocation.
///
/// It is split out because the token memo can be probed with the context alone.
/// An entry exists for an address only if `token_memo_put` accepted it, and that
/// call is itself guarded by the full `should_substitute` — so the entry's mere
/// existence already witnesses that this object's body was proven pure. The body
/// half therefore has to be recomputed only on a MISS, which is what lets the
/// probe run before the class-name/purity/method-resolve prologue instead of
/// after it (see `token_memo_get`).
pub(crate) fn substitution_context() -> bool {
    enabled()
        && KEY_DEPTH.with(Cell::get) > 0
        && RENDER_DEPTH.with(Cell::get) > 0
        && CHAIN_PURE.with(Cell::get)
}

/// Whether the render about to run may return a token instead of its text: only
/// inside a key derivation, only NESTED, and only when this body and every
/// enclosing body is proven pure.
///
/// The NESTING condition is what makes publication impossible. A depth-0 render
/// under a key scope is not necessarily the key's own: the derivation also runs
/// guest code (draining a map-shaped key, a salted `hashCode`/`equals` probe),
/// and a token handed to THAT code has no pure enclosing frame to contain it. So
/// the outermost render always returns real text — which by then already carries
/// tokens for its whole subtree, i.e. the 98 KB key is 130 bytes anyway.
pub(crate) fn should_substitute(body_pure: bool) -> bool {
    body_pure && substitution_context()
}

/// `RAVA_TOKEN_PROBE_FIRST` (default OFF) — probe the token memo BEFORE the
/// per-render class/purity/method-resolve prologue rather than after it.
///
/// The memo made the key derivation O(distinct nodes) of RENDERING, but left the
/// BOOKKEEPING at O(edges): every visit to an already-derived node still paid a
/// class-name `to_string`, a transitive purity proof and a name-keyed
/// `resolve_method` before reaching the O(1) lookup that discarded all of it.
/// Hoisting the probe is the project's standing rule applied to this path — a
/// per-call check belongs off the hot path, not on it.
pub(crate) fn probe_first() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| enabled() && std::env::var_os("RAVA_TOKEN_PROBE_FIRST").is_some())
}

/// Whether a map-key derivation is in progress on this thread. Renders produced
/// under one carry tokens, so the stringify memo must not share them with
/// ordinary renders of the same object.
pub(crate) fn in_key_scope() -> bool {
    enabled() && KEY_DEPTH.with(Cell::get) > 0
}

/// Identity of the world a memoised token was derived in. The GC epoch is what makes
/// an ADDRESS a stable name (between collections the bump allocator neither frees nor
/// relocates). The LOADER epoch is what makes the stored token's admission stable: an
/// entry is a standing claim that this object's `toString` was proven pure, and that
/// proof ranges over the loaded hierarchy — so a class load has to retire it. Only the
/// second is new, and only it makes probing before the purity proof exact rather than
/// merely usually-right.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TokenEpoch {
    pub(crate) gc: u64,
    pub(crate) loader: u64,
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// Cross-derivation token memo for ARENA objects, tagged with `TokenEpoch` — the
    /// discipline `map::valkey_memo` already uses for top-level keys. A collection or a
    /// class load bumps the tag and the table is dropped.
    /// Heap `Ref` handles are deliberately NOT memoised: mark-sweep recycles a handle
    /// without bumping the epoch, so an entry could alias a reused slot.
    ///
    /// This is what turns the derivation from O(roots x subtree) into O(distinct nodes):
    /// interning already made each cached entry a 9-byte token.
    static TOKENS: RefCell<(TokenEpoch, HashMap<u64, Box<str>>)> =
        RefCell::new((TokenEpoch::default(), HashMap::new()));
}

/// The token already derived for the arena object at `address` in `epoch`, if any.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn token_memo_get(address: u64, epoch: TokenEpoch) -> Option<String> {
    let hit = TOKENS.with(|cell| {
        let table = cell.borrow();
        (table.0 == epoch).then(|| table.1.get(&address).map(|t| t.to_string()))?
    });
    crate::residue_probe::token_probe(hit.is_some());
    hit
}

/// Remember `token` for the arena object at `address`. The caller must have checked
/// that NO collection ran during the render, so the address is still this epoch's.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn token_memo_put(address: u64, epoch: TokenEpoch, token: &str) {
    TOKENS.with(|cell| {
        let mut table = cell.borrow_mut();
        if table.0 != epoch {
            table.0 = epoch;
            table.1.clear();
        }
        table.1.insert(address, token.into());
    });
}

/// The token naming `text`. Fixed width, so no token is a prefix of another.
pub(crate) fn token(text: &str) -> String {
    let mut table = TABLE.get_or_init(Default::default).lock().unwrap();
    // PROBE BEFORE INSERT. `entry()` needs an OWNED key, so `text.into()` allocated a `Box<str>`
    // on every call - including the overwhelming majority that HIT and immediately dropped it
    // again. Tokens repeat by construction (that is the whole point of interning), so the hit path
    // is the hot one and it now allocates nothing.
    if let Some(&id) = table.get(text) {
        return format!("{MARK}{id:08x}");
    }
    let id = table.len() as u32;
    table.insert(text.into(), id);
    format!("{MARK}{id:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_text_shares_a_token_and_tokens_are_fixed_width() {
        let a = token("Ap2[argument1=x]");
        let b = token("Ap2[argument1=x]");
        let c = token("Ap2[argument1=y]");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.chars().count(), 9);
        assert_eq!(c.chars().count(), 9);
    }

    #[test]
    fn substitution_needs_a_key_scope_a_nesting_and_a_pure_chain() {
        assert!(!should_substitute(true), "no key scope");
        let _key = KeyScope::enter();
        let _root = RenderGuard::enter(true);
        if enabled() {
            assert!(should_substitute(true));
            assert!(!should_substitute(false), "an impure body never substitutes");
            let _impure = RenderGuard::enter(false);
            assert!(!should_substitute(true), "an impure frame poisons its subtree");
        }
    }

    /// The hoist rests on exactly one identity: splitting the predicate must not
    /// change it. `should_substitute(b)` has to stay `b && substitution_context()`
    /// for every `b` in every context, or probing on the context alone would admit
    /// a token where the historic path rendered text.
    #[test]
    fn the_context_half_composes_back_into_the_whole_predicate() {
        for body_pure in [true, false] {
            assert_eq!(should_substitute(body_pure), body_pure && substitution_context());
        }
        let _key = KeyScope::enter();
        let _root = RenderGuard::enter(true);
        for body_pure in [true, false] {
            assert_eq!(should_substitute(body_pure), body_pure && substitution_context());
        }
        let _impure = RenderGuard::enter(false);
        for body_pure in [true, false] {
            assert_eq!(should_substitute(body_pure), body_pure && substitution_context());
        }
    }

    /// A memoised token is only valid in the world it was derived in: EITHER epoch
    /// moving must retire it. The loader half is what lets the probe run before the
    /// purity proof — an entry stands for "this body was proven pure", and that proof
    /// ranges over the loaded hierarchy.
    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn a_token_is_only_served_back_in_its_own_epoch() {
        let epoch = TokenEpoch { gc: 4, loader: 9 };
        token_memo_put(0xBEEF, epoch, "\u{1}0000002a");
        assert_eq!(token_memo_get(0xBEEF, epoch).as_deref(), Some("\u{1}0000002a"));
        assert_eq!(token_memo_get(0xBEEF, TokenEpoch { gc: 5, loader: 9 }), None);
        assert_eq!(token_memo_get(0xBEEF, TokenEpoch { gc: 4, loader: 10 }), None);
        assert_eq!(token_memo_get(0xFACE, epoch), None);
    }

    #[test]
    fn the_size_threshold_is_a_pure_function_of_the_text() {
        assert!(!worth_interning("Marker[type=Interpolated]"));
        assert!(worth_interning(&"x".repeat(MIN_INTERN_BYTES)));
        // Equal texts always take the same branch, which is what keeps the
        // token/text mix injective.
        let text = "y".repeat(MIN_INTERN_BYTES - 1);
        assert_eq!(worth_interning(&text), worth_interning(&text.clone()));
    }
}
