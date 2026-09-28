# GAP-A01: Unloadable-Interface Closure — Design and Staging

Closing the last enumeration refusal in the offload admission gate: a virtual/interface
callee whose DECLARED OWNER is an unloadable interface name (`java/lang/Comparable`,
`java/util/List`) currently rejects the whole fork subtree unconditionally.

## 1. Target leaves

From `bench/canon_drill4_1.log.err` (canonical ChunkFill drill, root
`BlockState.initCache()V`), `Block.getFluidState` has 46 loaded concrete targets,
45 failing, and BOTH failure families bottom out at the same refusal shape:

- **`java/lang/Comparable.equals(Ljava/lang/Object;)Z`** — 44 targets, via
  `FlowingFluid.getSource` → `FluidState.setValue` → `StateHolder.setValueInternal`
  (`BlockState.setValueInternal` calls `Comparable.equals` on property values).
- **`java/util/List.get(I)Ljava/lang/Object;`** — 1 target (`LiquidBlock.getFluidState`).

Both die identically: `virtual_dispatch_closure_uncached` (lib.rs ~12180) returns
`None` for any owner that fails `get_or_load` (and for loadable interface owners),
and the only recovery — `unloadable_owner_contract_fallback` (lib.rs ~14635) —
refuses because neither `Comparable.equals` nor `List.get` has (or can soundly
have) a blanket Pure native contract keyed on the OWNER alone: the whole point of
these calls is that they dispatch into arbitrary implementors.

## 2. Design: `unloadable_iface_closure_admits(owner, name, desc)`

A new admission arm that ENUMERATES what the contract fallback could only refuse.
It is the conjunction of FOUR gates (all must pass), computed as an epoch-stable
fixed point (same loop discipline as `virtual_dispatch_closure`: recompute until
`loader.epoch()` is unchanged across one full pass), memoised in a new cache that
`offload_epoch_sync` (lib.rs ~12140) clears alongside its siblings.

Wired as a DISJUNCT at the four closure-miss arms — the `None =>` branches in
`offload_reason` (~14299), `offload_reason_devirt` (~14327), `offload_drill_reason`
(~14521) and `is_offloadable_rec` (~14756) — factored into one
`closure_miss_admits(owner, name, desc)` helper so the four arms cannot drift:
`unloadable_owner_contract_fallback(..) || unloadable_iface_closure_admits(..)`.

The soundness claim being assembled: *every possible runtime receiver of the call
is either (P1) a loaded class the analysis enumerated and proved, (P2) a lambda —
refuted, (P3) a natively-minted unloadable-class object — covered by an audited
registry, (P4) a bare `Value` dispatched through the wrapper map — covered by a
pinned table, or null (NPE — path-terminal, same as today).*

## 3. Prerequisite: `opcodes::name_may_subtype`

Third variant beside `name_is_subtype` / `name_is_subtype_strict` (opcodes.rs
~1664/~1697): an inconclusive hierarchy chain (any walk step that is unloadable AND
absent from `JDK_SUPERS`) answers **UNCONDITIONALLY true**. Enumeration must
OVER-include. The existing lenient walk's fallback
(`inconclusive && jdk_supers(sup).is_none() && get_or_load(sup).is_err()`) is
correct for its REFUTATION consumer (`unloadable_owner_closed`) but UNDER-includes
for an enumerator whenever `sup` is a `JDK_SUPERS` key like `java/util/List`: a
loaded implementor whose chain dies at an unknown JDK boundary would be silently
dropped from the closure — an unsoundly missing dispatch target, not a mere
over-rejection. Direction table:

| variant                | inconclusive chain answers | consumer                     |
|------------------------|----------------------------|------------------------------|
| `name_is_subtype`      | yes iff sup unknown+unloadable | refutation (closed-check) |
| `name_is_subtype_strict` | no                       | `Class.isAssignableFrom`     |
| `name_may_subtype`     | **always yes**             | closure ENUMERATION (Gate 1) |

## 4. Gate 1 — loaded implementors all pass

`Vm::interface_dispatch_closure(owner, name, desc)`: mirror of
`virtual_dispatch_closure_uncached` (lib.rs ~12180-12218) with two deltas:
**no owner load** (the owner is typically unloadable; a loadable interface owner
uses the same path), and the subtype filter is `name_may_subtype(cn, owner)`
instead of the `super_chain` walk. Same skeleton otherwise: concrete classes only
(skip `ACC_INTERFACE|ACC_ABSTRACT`), dedup by resolved `Code` pointer, a
no-bytecode resolution stays as its own representative. Gate 1 passes iff EVERY
representative passes `is_offloadable_rec`.

Two STRENGTHENINGS required before this gate can carry admission weight:

1. **`force_native_collection` interception** (opcodes.rs:392): a class/method the
   runtime force-routes to natives (netty event loops, `BufferedInputStream`,
   guava `HashBiMap`/`MapMaker`/…) never executes the bytecode the analyzer would
   analyse. Such a target must NOT be admitted via bytecode analysis — it needs an
   exact native contract or it rejects. **Audit item: the existing class-owner
   `virtual_dispatch_closure` consumers have the same latent hole** — fix both.
2. **Inherited `Object.equals` mechanical contract**: most Gate-1 `equals` targets
   don't override `equals` at all; resolution finds no bytecode and the rep would
   die as NATIVE/unresolved. Add `Vm::inherited_object_method_admissible(class,
   name, desc)`, mirroring the shape of `private_throwable_ctor_admissible`
   (lib.rs ~14597): admissible iff (a) NO bytecode anywhere in the resolution walk,
   (b) the class is LOADED (its file is the authority that no override exists),
   (c) `(name, desc) == ("equals", "(Ljava/lang/Object;)Z")`. The native service
   is then the audited pure identity arm at natives.rs:1082
   (`Object.equals` → `ref_eq`) — reads nothing, writes nothing.

## 5. Gate 2 — lambda refutation

`virtual_dispatch_closure` refuses interface owners because lambdas implement
interfaces without appearing in the loader. Refute instead of refuse: per epoch,
compute `lambda_sam_ifaces()` — a static scan of ALL loaded constant pools for
`Constant::InvokeDynamic` whose bootstrap method owner is
`java/lang/invoke/LambdaMetafactory` (`metafactory` AND `altMetafactory`); the SAM
type is the indy site descriptor's RETURN type. Gate 2 rejects the closure if any
scanned SAM `name_may_subtype`s the owner (a lambda receiver could exist; its body
is not enumerable).

Coverage argument: production lambdas are minted at exactly ONE site —
`Plan::Lambda` → `heap.alloc_lambda` (opcodes.rs:3770). Capturing link-time minted
sites alone would be UNSOUND (a lambda could be minted for the first time inside
the fork region, after the check); the static pool scan covers UNLINKED sites too,
so it is mint-order-independent. The `"$Lambda"` heap class name (heap.rs:2150)
cannot collide with Gate 1's loader enumeration (`$Lambda` is not a loadable name).

## 6. Gate 3 — native-minted receivers + value dispatch

Two fixed tables in `native_contract.rs`:

- **`VALUE_DISPATCH_CLASSES`** — pinned to the interface-receiver wrapper-routing
  map at opcodes.rs:3352-3358: `String`/`Integer`/`Long`/`Float`/`Double` (a bare
  `Value::Str`/`Int`/… receiver dispatches as its wrapper class). Note the
  asymmetries: a boxed int IS `Value::Int` (`Integer.valueOf` is identity,
  natives.rs:947), but `Boolean.valueOf` mints a REAL heap object
  (natives.rs:973) — Boolean belongs to NATIVE_MINTED, not here.
- **`NATIVE_MINTED`** — the audited registry of unloadable class names rava's own
  natives can mint heap objects under (~95 names: `ArrayList`, `HashMap`,
  `Map$Entry`, `Iterator`, `Comparator`, `Boolean`, `BigInteger`, the
  nio/net/reflect families, …).

For each table entry X that `name_may_subtype`s the owner: require a **Pure,
non-callback** contract for `(X, name, desc)` — otherwise reject. Every entry
also needs a `JDK_SUPERS` row so the may-subtype walk is authoritative over the
registry (add missing rows: a `java/lang/Comparable` root, `java/util/Map$Entry`,
`java/util/Comparator`, `java/util/Optional`, `java/math/BigInteger` →
`[Number, Comparable]`, …) — plus a ROW-COVERAGE TEST: every NATIVE_MINTED /
VALUE_DISPATCH name resolves in JDK_SUPERS.

## 7. Gate 4 — intern-pool refutation + mint-epoch

Backstop against a native mint the registry missed: reject if the heap's
class-name intern pool contains any UNLOADABLE, non-`"$Lambda"`, non-registry
name that `name_may_subtype`s the owner. `Heap::intern` (heap.rs:1797) is the
single choke point — every heap object class name passes through it, including
`reflect.rs:753` mints (`reflect::new_instance` → `heap.alloc(cls)`).

New `Heap::mint_epoch`, bumped on FIRST insertion of a name into the pool.
`offload_epoch_sync` compares the COMPOSITE `(loader.epoch, heap.mint_epoch)` and
clears all closure caches when either moves — a post-verdict first-mint of a new
unloadable name invalidates cached admissions.

## 8. Mid-region soundness (verdict derived BEFORE the region runs)

- Serial divert: trivially sound (same thread, GIL held, any miss re-analyses).
- Parallel: fenced by the existing worker abort gates (a gil-free worker cannot
  load classes — a loader miss aborts the region; see `gilfree_thread_cannot_load_classes`).
- Residual 1 — the subtree's own `new X`: the analyzer sees `0xbb` (offload.rs
  ~804, currently just `F::Fresh(cp)`) — issue a **MINT QUERY** `CalleeQuery`
  (mint-kind) naming X. Handler: loadable X → load it NOW (the epoch bump makes
  Gate 1 enumerate it); unloadable X → must be NATIVE_MINTED-covered or reject.
- Residual 2 — Pure-contract natives that ALLOCATE: their mints must be ⊆
  NATIVE_MINTED (doc + test audit of the contract table).

## 9. Drill integration

Extend the `offload_drill_node` closure-miss `None` arm (lib.rs ~14399-14427) to
name the FAILING GATE instead of today's flat "UNLOADABLE [no pure contract]":
Gate 1 → enumerate `interface_dispatch_closure` and reuse the existing group-by
machinery (group failing targets by one-level reason, drill one representative per
group); Gate 2 → print the offending SAM + its declaring class; Gate 3 → the
registry entry with the missing contract; Gate 4 → the rogue interned name.

## 10. Soundness summary per receiver population

- **P1 loaded classes**: Gate 1 enumerates every loaded concrete may-subtype
  (over-inclusive walk) and proves each rep; strengthenings close the
  force-native and inherited-Object.equals holes.
- **P2 lambdas**: Gate 2 refutes by static SAM scan over ALL pools (unlinked
  sites included).
- **P3 unloadable-class heap objects**: only rava natives can mint them; Gate 3
  covers the audited registry, Gate 4 catches any name outside it at the intern
  choke point (window: see risks).
- **P4 bare values**: the dispatch map is pinned by VALUE_DISPATCH_CLASSES.
- **Null**: NPE — path-terminal, unchanged.
- Adversarial renaming: inherited from GAP-S03 — resolvable guest bytecode is
  always analysed, never name-trusted (contracts require unloadable/no-bytecode).

## 11. Stages

1. **`name_may_subtype` + `interface_dispatch_closure` + DRILL-ONLY wiring.**
   Diagnostics only, zero admission change. Capture the REAL Gate-1 failing list
   for `Comparable.equals` / `List.get` — this decides viability (see risks).
2. Lambda SAM scan (Gate 2) + its drill arm.
3. Tables (VALUE_DISPATCH_CLASSES / NATIVE_MINTED) + JDK_SUPERS rows + row-coverage
   test + mint-epoch (Gate 4) + contracts + the force_native strengthening (both
   closures) + `inherited_object_method_admissible`.
4. Mint query (0xbb CalleeQuery) + gate assembly (`unloadable_iface_closure_admits`)
   + `closure_miss_admits` factoring + flip-on.
5. Canonical validation: hash gate `8790f04ace726ecf` (ChunkFill 1 64). The change
   is ANALYSIS-ONLY — any hash movement is a bug, full stop. Payoff cap: this
   closure only widens SERIAL-DIVERT admission for now, because
   `method_fork_safe` forbids indy/athrow on workers — the parallel win lands
   only after those worker gates are themselves widened.

## 12. Honest risks

- **Gate-1 breadth**: `Comparable` has hundreds of loaded implementors; ONE
  handler-bearing (or otherwise rejecting) `equals` body rejects the leaf for
  EVERY caller. If the measured failing set is broad/diverse, the answer is not
  more contracts — it is design (a): flow-sensitive receiver typing at the call
  site (track that the value flowing into `Comparable.equals` is a property value
  of a known concrete type), a different and larger design.
- **Registry completeness is an audit, not a proof.** Gate 4 narrows the exposure
  to one epoch-sync-free stretch (a first-mint between the last sync and region
  entry); acceptable for serial-divert, must be re-examined before parallel.
- **May-subtype over-rejection noise**: every inconclusive-chain loaded class
  lands in every closure; harmless for soundness, but it can bloat Gate 1 with
  targets that were never real receivers (analysis cost + false rejects).
- **Abstract-class assumption inherited**: like the sibling closure, abstract
  classes are never exact receivers — relies on instantiation soundness elsewhere.
- **Defensive depth**: `reflect::new_instance` can mint under a LOADABLE class
  name without the analyzer seeing a `new` — covered today because loading is
  epoch-visible, but a defensive check there is cheap and should eventually land.
