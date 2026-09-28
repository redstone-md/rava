// VoxelShape CSG — the boot's largest NAMED concentration inside Bootstrap.bootStrap()
// (== ChunkFill's SETUP_MS), isolated so it can be A/B'd against the JVM without a boot.
//
// WHAT IT MIRRORS. WallBlock.makeShapes(f, g) builds a post + four side boxes and then,
// for every one of its 162 block states (UP boolean x 4 directions x 3 WallSide values),
// folds the sides into an accumulator with Shapes.or. The profile puts 6.83 s / 52 calls
// there — ~131 ms per makeShapes, ~810 us per state. This bench reproduces exactly that
// inner shape: 162 states, four Shapes.or per state, the same vanilla wall geometry
// (Block.column(8,0,f) for the post and Block.boxZ(6,0,h,0,11) rotated horizontally for
// the sides, expanded here to literal Shapes.box coordinates so no Block/registry class
// has to initialise). Both (f,g) pairs a WallBlock actually constructs are used: (16,14)
// for the outline shape and (24,24) for the collision shape.
//
// ON WORKING-SET SIZE. BigGraph's lesson is that an L1-resident probe cannot measure
// object PLACEMENT. That lesson does not invert into "make everything big": the quantity
// under test here is the cost of a single small CSG, and the real workload's own working
// set is a handful of DoubleLists and a small BitSet. Inflating it would measure a
// different thing than the boot does. What this probe must avoid instead is measuring a
// CACHE: every state's fold is a fresh Shapes.or chain (VoxelShape memoises nothing), and
// the state loop is the full 162-state cross product, not one shape repeated.
//
// Coordinates stay on the 1/16 grid, which is what keeps Shapes.findBits/lcm from
// exploding the merged index space — off-grid coordinates would make this a harder CSG
// than the boot ever performs.
//
//   javac -cp <mc-cp> -d out ShapeOr.java
//   java  -cp out;<mc-cp> ShapeOr [passes] [warmup]
//   rava  -cp out;<mc-cp> ShapeOr [passes] [warmup]
// SUM is a bit-exact gate (a hash over every resulting shape's bounds); HOTNS is the ratio.
// bench/shapeor_ab.ps1 runs the interleaved vs-JVM comparison; shapeor_flag_ab.ps1 A/Bs a flag.
//
// MEASURED (2026-07-25, i5-14400F P-cores, medians of 3): rava 28.1 s vs jvm 375 ms for 200
// passes = 75x, spread 67-124x. Fidelity check: 123-141 ms per 162-state pass here against the
// boot profile's 131 ms per WallBlock.lambda$makeShapes$0 — the probe reproduces the real cost.
// RAVA_JIT_PIC_PROBE reports 13.89M dynamic calls out of compiled code per 25920 Shapes.or
// (536 per or), of which 4.10M fall to the Rust host and 3.15M of those are LAMBDA receivers
// the class-keyed heap-Ref PIC declares permanently never-cacheable (3.54M negatives, 38 fills
// in the whole run). Natives are only 0.4 s of 5.2 s (RAVA_BRIDGE_STAT) and every hot method
// compiles (compile_fail=0), so neither the native bridge nor a JIT bailout is the cost.
// RAVA_LAMBDA_PIC is INERT on this workload: with the flag on the dispatch census is
// byte-identical (same 9792602 hits / 3548627 misses / 38 fills) and the time moves +0.8%.
//
// CORRECTED 2026-07-26 — this file used to conclude "the cause is dispatch, not arithmetic",
// reading RAVA_PROFILE_WALL's 87-90% JIT_EXEC as compiled-code throughput. Both halves were
// wrong, and RAVA_PROFILE_PC (RIP sampling) shows why: only 6.5-7.4% of samples are INSIDE
// Cranelift-compiled code. The other ~93% is the Rust host. The category sampler raises its
// JIT_EXEC guard around the whole of `jit_run`, so every callback from compiled code back into
// Rust that does not raise its own category — the dispatch host path, the arena/field helpers,
// the allocator — is billed to JIT_EXEC while no machine code is running.
// The host side, by function: `Heap::lambda_fp` 10.2% (the capturing-lambda content
// fingerprint, refolded per dispatch), `Vm::jit_direct_invoke` 9.9%, name-keyed hashing
// ~14% (`tlcache::field_key_get` over a `HashMap<(String,String),u32>`, SipHash, memchr,
// memcmp), `ClassLoader::get_or_load` 4.0%, mimalloc ~7%, and ~2.5% in
// RtlQueryEnvironmentVariable from an unmemoised `std::env::var_os` on a per-call path.
// So the workload is not dispatch-COUNT bound and not codegen bound: it is bound by the
// per-call bookkeeping the host does, which is O(string) and O(malloc).
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

public class ShapeOr {
    private static final double S = 1.0 / 16.0;

    // Block.column(8, 0, f) — the wall post.
    static VoxelShape post(double f) {
        return Shapes.box(4 * S, 0.0, 4 * S, 12 * S, f * S, 12 * S);
    }

    // Shapes.rotateHorizontal(Block.boxZ(6, 0, h, 0, 11)) — the four side boxes.
    static VoxelShape[] sides(double h) {
        double t = h * S;
        return new VoxelShape[] {
            Shapes.box(5 * S, 0.0, 0.0,   11 * S, t, 11 * S), // NORTH
            Shapes.box(5 * S, 0.0, 5 * S, 11 * S, t, 16 * S), // SOUTH
            Shapes.box(0.0,   0.0, 5 * S, 11 * S, t, 11 * S), // WEST
            Shapes.box(5 * S, 0.0, 5 * S, 16 * S, t, 11 * S), // EAST
        };
    }

    // One makeShapes() worth of work: 162 states x 4 Shapes.or.
    static long pass(VoxelShape post, VoxelShape[] low, VoxelShape[] tall) {
        long h = 1;
        for (int up = 0; up < 2; up++) {
            for (int code = 0; code < 81; code++) {   // 3^4 side states
                VoxelShape s = up == 1 ? post : Shapes.empty();
                int c = code;
                for (int d = 0; d < 4; d++) {
                    int w = c % 3;
                    c /= 3;
                    VoxelShape side = w == 0 ? Shapes.empty() : (w == 1 ? low[d] : tall[d]);
                    s = Shapes.or(s, side);
                }
                if (s.isEmpty()) {
                    h = h * 1000003L + 7777L;
                } else {
                    AABB b = s.bounds();
                    h = h * 1000003L + Double.doubleToRawLongBits(b.minX);
                    h = h * 1000003L + Double.doubleToRawLongBits(b.minY);
                    h = h * 1000003L + Double.doubleToRawLongBits(b.minZ);
                    h = h * 1000003L + Double.doubleToRawLongBits(b.maxX);
                    h = h * 1000003L + Double.doubleToRawLongBits(b.maxY);
                    h = h * 1000003L + Double.doubleToRawLongBits(b.maxZ);
                }
            }
        }
        return h;
    }

    // The two (f, g) pairs a real WallBlock constructs: outline then collision.
    private static final double[][] FG = { { 16.0, 14.0 }, { 24.0, 24.0 } };

    static long round(int passes) {
        long h = 1;
        for (int i = 0; i < passes; i++) {
            double[] fg = FG[i % FG.length];
            VoxelShape p = post(fg[0]);
            VoxelShape[] tall = sides(fg[0]);
            VoxelShape[] low = sides(fg[1]);
            h = h * 31L + pass(p, low, tall);
        }
        return h;
    }

    public static void main(String[] args) {
        int passes = args.length > 0 ? Integer.parseInt(args[0]) : 200;
        int warmup = args.length > 1 ? Integer.parseInt(args[1]) : 20;

        long w = round(warmup);
        long t0 = System.nanoTime();
        long h = round(passes);
        long ns = System.nanoTime() - t0;

        System.out.println("SUM=" + Long.toHexString(h) + " WARM=" + Long.toHexString(w)
            + " PASSES=" + passes + " ORS=" + ((long) passes * 162 * 4));
        System.out.println("HOTNS=" + ns);
    }
}
