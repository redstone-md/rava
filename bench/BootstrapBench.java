// THE BOOT-CRITICAL INSTRUMENT: Minecraft's own registry + block-state initialiser, standalone.
//
// WHY. A full server boot on this box has a ~8% noise floor and every change this month is 1-4%;
// bench/cpu_ab.ps1 reads 1.2% arm spread but only on synthetic programs that do not contain the boot
// population. The wall-profile of a real boot puts 41% of JIT_EXEC in three methods that all run
// INSIDE Bootstrap.bootStrap(): BlockBehaviour$BlockStateBase.initCache (driven from
// Blocks.<clinit>, javap pc 40085), WallBlock.lambda$makeShapes$0, StateHolder.lambda$populateNeighbours$0.
// This program is that call and nothing else, timed inside the guest, with a content hash so two
// engines can be checked for AGREEMENT as well as speed.
//
// WHAT IT CALLS, in the order net.minecraft.server.Main.main does (javap -c Main: pc 0 and pc 268):
//   SharedConstants.tryDetectVersion()   -- reads /version.json from the server jar; untimed here
//   Bootstrap.bootStrap()                -- pc 15 getstatic BuiltInRegistries.REGISTRY triggers the
//                                           whole registry <clinit> chain (Blocks, Items, ...),
//                                           then FireBlock/ComposterBlock/EntitySelectorOptions/
//                                           DispenseItemBehavior/CauldronInteraction/
//                                           BuiltInRegistries.bootStrap, CreativeModeTabs.validate,
//                                           wrapStreams()
// Pure Minecraft bytecode drives Minecraft's own bootstrap; this file only times and folds.
//
// ⚠ NOT IDEMPOTENT. Bootstrap.bootStrap() begins `getstatic isBootstrapped; ifeq; return` (pc 0-6),
// so a second call in the same process is a no-op and cannot be timed. Exactly ONE call is timed per
// process; repeat the PROCESS for N samples. (The hash fold, which is read-only, may be repeated
// with `--fold N` to give the JIT a warm read of the state graph, but that is not the bootstrap.)
//
// ⚠ Bootstrap.bootStrap() ends with wrapStreams(): System.out is replaced by a log4j-backed stream,
// so the result line is written to the PrintStream captured BEFORE the call.
//
// OUTPUT (exactly one line on the original stdout):
//   BOOTSTRAP_MS=<ms> STATES=<count> HASH=<16 hex>
// HASH = FNV-1a 64 over, for every Block in BuiltInRegistries.BLOCK (iteration order = numeric id):
//   registry key, numeric id, then for every state in getStateDefinition().getPossibleStates():
//   its property=value entries SORTED BY PROPERTY NAME (not map order), whether it is the block's
//   default state, and six cache-derived scalars that initCache() computed
//   (getLightEmission, canOcclude, propagatesSkylightDown, getLightBlock, hasLargeCollisionShape,
//   isSolidRender). Agreement on HASH therefore certifies the registry, the state graph AND initCache.
//
// Usage:  java -cp out;<bench\mc-cp.txt> BootstrapBench [--fold N]
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;

public class BootstrapBench {
    private static final long FNV_OFFSET = 0xcbf29ce484222325L;
    private static final long FNV_PRIME = 0x100000001b3L;

    /** Deterministic FNV-1a 64 fold. Encapsulates the hash so callers cannot mix in order-dependent noise. */
    private static final class Fold {
        private long h = FNV_OFFSET;
        private long states;

        void str(String s) {
            byte[] b = s.getBytes(StandardCharsets.UTF_8);
            for (byte x : b) { h ^= (x & 0xff); h *= FNV_PRIME; }
            h ^= 0xfe; h *= FNV_PRIME; // terminator so "ab","c" != "a","bc"
        }
        void i32(int v) { for (int k = 0; k < 4; k++) { h ^= (v >>> (8 * k)) & 0xff; h *= FNV_PRIME; } }
        void bool(boolean v) { h ^= v ? 1 : 2; h *= FNV_PRIME; }
        long hash() { return h; }
        long states() { return states; }

        @SuppressWarnings({"unchecked", "rawtypes"})
        void state(BlockState s, boolean isDefault) {
            states++;
            List<String> entries = new ArrayList<>();
            for (Map.Entry<Property<?>, Comparable<?>> e : s.getValues().entrySet()) {
                Property p = e.getKey();
                entries.add(p.getName() + '=' + p.getName((Comparable) e.getValue()));
            }
            entries.sort(null);
            i32(entries.size());
            for (String e : entries) str(e);
            bool(isDefault);
            // Cache-derived scalars: computed by BlockStateBase.initCache(), the 21% method.
            i32(s.getLightEmission());
            bool(s.canOcclude());
            bool(s.propagatesSkylightDown());
            i32(s.getLightBlock());
            bool(s.hasLargeCollisionShape());
            bool(s.isSolidRender());
        }

        void block(Block b) {
            str(BuiltInRegistries.BLOCK.getKey(b).toString());
            i32(BuiltInRegistries.BLOCK.getId(b));
            BlockState def = b.defaultBlockState();
            for (BlockState s : b.getStateDefinition().getPossibleStates()) state(s, s == def);
        }
    }

    private static Fold foldAll() {
        Fold f = new Fold();
        for (Block b : BuiltInRegistries.BLOCK) f.block(b);
        return f;
    }

    public static void main(String[] args) {
        int folds = 1;
        boolean dump = false;      // --dump: one line per block  <id> <key> <per-block hash>
        String dumpBlock = null;   // --dump-block <key>: one line per state of that block
        boolean validate = false;  // --validate: also run Bootstrap.validate(), the server's next call
        for (int i = 0; i < args.length; i++) {
            if ("--fold".equals(args[i]) && i + 1 < args.length) folds = Integer.parseInt(args[++i]);
            else if ("--dump".equals(args[i])) dump = true;
            else if ("--dump-block".equals(args[i]) && i + 1 < args.length) dumpBlock = args[++i];
            else if ("--validate".equals(args[i])) validate = true;
        }
        PrintStream realOut = System.out; // wrapStreams() replaces System.out inside bootStrap()

        SharedConstants.tryDetectVersion(); // Main pc 0; untimed, reads version.json from the jar

        long t0 = System.nanoTime();
        Bootstrap.bootStrap();               // Main pc 268; guarded by isBootstrapped -- ONE call only
        long t1 = System.nanoTime();
        if (validate) {
            // The server's very next call after bootStrap(); a boot that hangs between the oshi
            // report and `Environment:` is in one of the two, and the bench covered only the first.
            long v0 = System.nanoTime();
            Bootstrap.validate();
            realOut.println("VALIDATE_MS=" + (System.nanoTime() - v0) / 1_000_000L);
        }

        // LOCALISATION MODES. The first rava run disagreed with java on HASH while agreeing on
        // STATES, so the divergence is inside one or more blocks' state lists; these two modes let
        // the two engines' outputs be diffed line by line instead of compared as one 64-bit number.
        if (dump) {
            for (Block b : BuiltInRegistries.BLOCK) {
                Fold g = new Fold();
                g.block(b);
                realOut.println(BuiltInRegistries.BLOCK.getId(b) + " " + BuiltInRegistries.BLOCK.getKey(b)
                        + " " + String.format("%016x", g.hash()));
            }
            realOut.flush();
            return;
        }
        if (dumpBlock != null) {
            for (Block b : BuiltInRegistries.BLOCK) {
                if (!BuiltInRegistries.BLOCK.getKey(b).toString().equals(dumpBlock)) continue;
                BlockState def = b.defaultBlockState();
                int n = 0;
                for (BlockState s : b.getStateDefinition().getPossibleStates()) {
                    List<String> entries = new ArrayList<>();
                    for (Map.Entry<Property<?>, Comparable<?>> e : s.getValues().entrySet()) {
                        @SuppressWarnings({"unchecked", "rawtypes"}) Property p = e.getKey();
                        @SuppressWarnings({"unchecked", "rawtypes"}) String v = p.getName((Comparable) e.getValue());
                        entries.add(p.getName() + '=' + v);
                    }
                    entries.sort(null);
                    realOut.println((n++) + " " + entries + " def=" + (s == def)
                            + " light=" + s.getLightEmission() + " occl=" + s.canOcclude()
                            + " sky=" + s.propagatesSkylightDown() + " lb=" + s.getLightBlock()
                            + " large=" + s.hasLargeCollisionShape() + " solid=" + s.isSolidRender());
                }
            }
            realOut.flush();
            return;
        }

        Fold f = foldAll();
        for (int i = 1; i < folds; i++) {
            Fold g = foldAll();
            if (g.hash() != f.hash()) throw new IllegalStateException("fold not deterministic in-process");
        }
        realOut.println("BOOTSTRAP_MS=" + (t1 - t0) / 1_000_000L
                + " STATES=" + f.states()
                + " HASH=" + String.format("%016x", f.hash()));
        realOut.flush();
    }
}
