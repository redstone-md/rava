# RAVA_LAMBDA_IMPL_KEY — dispatch keyed on a lambda's IMPLEMENTATION, not its receiver

Status: EXPERIMENTAL, default OFF. Measured 2026-07-25 at commit 3417063.
Probe: `bench/ShapeOr.java` (the `WallBlock.makeShapes` VoxelShape CSG, ~5 s/side).

## The problem it removes

Every dispatch cache in RAVA keys on the RECEIVER. For a capturing lambda the
receiver is a fresh object per instantiation, so the key never repeats:
`BitSetDiscreteVoxelShape.join` mints three nested closures per `joinUnoptimized`,
and `forMergedIndexes` calls `IndexConsumer.merge` on a fresh closure per merged
index. The implementation those closures target is identical every time.

The direct-call cache already worked around this with `Heap::lambda_fp` — a 64-bit
FNV fold over `kind + owner + name + descriptor + captured.len()`. That is the right
IDEA (impl identity) with the wrong ENCODING:

* it hashes ~120 bytes of strings **on every dispatch**, and does it **twice** (once
  in `jit_direct_probe` to build the key, once in `jit_direct_invoke` to re-check the
  handle) — a dependent rotate/xor/multiply chain, not a load;
* it is a HASH, so two distinct implementations could in principle collide onto one
  cached call target — the silent-corruption class this project gates against.

## The identity used instead

`Heap::lambda_site_key(handle)` = the `Arc<LambdaSite>` address, tagged with bit 63.

Soundness, in the order it has to hold:

1. **Invariant across instantiations.** The `Arc<LambdaSite>` is minted ONCE per
   `invokedynamic` instruction (`opcodes::invokedynamic` → `Plan::Lambda`, cached per
   site by `tlcache::indy_put`) and cloned into every closure born there. Same site ⇒
   same address, whatever the captures.
2. **Distinct per site.** Each indy site does its own `Arc::new`, so the address
   implies (owner, name, descriptor) AND the MethodHandle kind AND the capture arity
   (the indy descriptor's parameter count is a constant of the instruction). That is
   everything `jit_direct_resolve` bakes into a `DirectEntry` except the captured
   VALUES. Content-equal sites get distinct keys — never merged.
3. **Captures are not implied, and are not cached.** `jit_direct_invoke` re-reads them
   from the live receiver on every call (`Heap::lambda_captures`, forward-barriered and
   `box_ref`'d per call); only the capture COUNT and the per-slot ref MASK are baked,
   both properties of the impl descriptor. The `captured.len() != cap_n` check stays as
   a hard backstop rather than an assumption.
4. **Impl identity does NOT imply the target for an unbound method ref** (kind 5/9):
   there the SAM's arg0 IS the receiver and the callee depends on its dynamic class.
   That case keeps its per-call `arg0_guard` re-check, unchanged. Bound/ctor refs
   (kind 7 and friends) still take the general path.
5. **The address cannot be recycled.** The canonical `Arc` lives in a THREAD-LOCAL
   indy cache, so a worker thread's exit could drop it and free the allocation. Before
   an address is ever used as a cache key, `nextgen::lambda_site_pin` puts a strong
   reference in a process-lifetime, pointer-keyed table (cold resolve path only). A
   live allocation cannot share an address with another live allocation, so a pinned
   key can never be re-minted onto a different site.
6. **Disjoint from every other guard** sharing the direct-call cache's key space:
   interned class-name pointers (< 2^48), arena class-id+1 (small), and `REF_TAG|handle`
   identity cells (< 2^41) all lack bit 63.

## Measured (i5-14400F P-cores, High-Performance plan, interleaved)

| gate | OFF | ON |
|---|---|---|
| ShapeOr 40 passes, median of 3, ABAB | 4842 ms | **3677 ms (-24.1%)** |
| ShapeOr 200 passes vs JVM, median of 3 | 26567 ms | **20071 ms (-24.4%)** |
| ChunkFill `SETUP_MS`, median of 3 | 80928 | **68220 (-15.7%)** |
| ChunkFill `FILL_MS`, median of 3 | 25886 | **23481 (-9.3%)** |
| ChunkFill wall, median of 3 | 107.5 s | **92.2 s (-14.2%)** |

Gates held: `SUM=3cfa327f01e0c901` bit-exact on every ShapeOr run both arms;
`ChunkFill_HASH=8790f04ace726ecf` on all six ChunkFill runs both arms;
`cargo test -p rava-interp --release --features next-gen-gc` = 366 passed / 0 failed;
`corpus/conformance` identical output OFF and ON on every test.

## What it does NOT do

The PIC census is BYTE-IDENTICAL with the flag on: 13889279 dynamic calls, 9792602
machine-code PIC hits, 4096677 host falls, 3154796 lambda-receiver falls, `cache
fills=38 never-cacheable=3544456`. This lever cuts the COST of a host fall, not the
COUNT. See `docs/lambda-machine-code-pic-blocker.md` for why the machine-code arm
cannot be reached from here without a capture-representation change.
