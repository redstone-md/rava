// THE DATAPACK-RELOAD INSTRUMENT: the window between `Environment:` and `Loaded 1470 recipes`,
// standalone, headless, one process, no world, no network.
//
// WHY. BootstrapBench covers start -> `Environment:` (~17 s of a 27 s rava boot). The NEXT window,
// `Environment:` -> `Loaded 1470 recipes`, is 13-17 s on rava vs 2 s on java and had no instrument.
// It is Minecraft's datapack reload: WorldLoader.load(...) over the vanilla server pack -- tags,
// worldgen + dimension registries via RegistryDataLoader, then ReloadableServerResources
// (loot tables / predicates / item modifiers, recipes, advancements, functions, commands) driven by
// SimpleReloadInstance on Util.backgroundExecutor() plus a game executor the main thread drains.
//
// WHAT IT CALLS, in the order net.minecraft.server.Main.main does (javap -c Main, 1.21.11):
//   pc 0    SharedConstants.tryDetectVersion()          -- untimed
//   pc 268  Bootstrap.bootStrap()                       -- untimed (BootstrapBench's population)
//   pc 683  ServerPacksSource.createPackRepository(LevelStorageAccess)
//             = new PackRepository(new ServerPacksSource(validator),
//                                  new FolderRepositorySource(<world>/datapacks, ...))
//           HERE: ServerPacksSource.createVanillaTrustedRepository() -- the same ServerPacksSource,
//           minus the FolderRepositorySource over a world's (empty) datapacks folder. NO WORLD.
//   pc 699  loadOrCreateConfig(props, dynamic=null, safeMode=false, repo)   (javap: pc 20-70)
//             cfg = new WorldDataConfiguration(props.initialDataPackConfiguration, FeatureFlags.DEFAULT_FLAGS)
//                   -- default `initial-enabled-packs=vanilla`, i.e. DataPackConfig.DEFAULT
//             new PackConfig(repo, cfg, safeMode=false, initMode=true)
//             new InitConfig(packConfig, CommandSelection.DEDICATED, props.functionPermissions)
//                   -- default `function-permission-level` = LevelBasedPermissionSet.GAMEMASTER
//   pc 721  Util.blockUntilDone(executor -> WorldLoader.load(initConfig, supplier, WorldStem::new,
//                                                            Util.backgroundExecutor(), executor))
//           blockUntilDone (javap -c Util) = LinkedBlockingQueue; executor = queue::add;
//           while (!future.isDone()) { r = queue.poll(100 ms); if (r != null) r.run(); }
//           -- that queue-drain IS the dedicated server's "game executor" for this window and is
//           mirrored here verbatim (GameQueue), on the calling thread.
//   WorldLoader.load body (javap -c WorldLoader, pc 0-230), replayed inline so it can be timed:
//     pc 4    packConfig.createResourceManager()  -> MinecraftServer.configurePackRepository
//                                                    (PackRepository.reload, select "vanilla"),
//                                                    openAllSelected, new MultiPackResourceManager(SERVER_DATA)
//     pc 19   RegistryLayer.createRegistryAccess()
//     pc 34   TagLoader.loadTagsForExistingRegistries(rm, layers.getLayer(STATIC))
//     pc 44   layers.getAccessForLoading(WORLDGEN)
//     pc 53   TagLoader.buildUpdatedLookups(that, pendingTags)
//     pc 65   RegistryDataLoader.load(rm, lookups, RegistryDataLoader.WORLDGEN_REGISTRIES)
//     pc 87   lookups2 = concat(lookups, worldgen.listRegistries()).toList()
//     pc 101  RegistryDataLoader.load(rm, lookups2, RegistryDataLoader.DIMENSION_REGISTRIES)
//     pc 123  HolderLookup.Provider.create(lookups2.stream())
//     pc 144  supplier.get(new DataLoadContext(rm, cfg, provider, dimensions))
//               Main.lambda$main$0 with dynamic==null -> Main.createNewWorldData(settings, ctx,
//               levelStems, demo=false, bonusChest=false)  (javap: pc 26-184):
//               LevelSettings("world", SURVIVAL, hardcore=false, EASY, false, new GameRules(features), cfg)
//               props.worldOptions = new WorldOptions(randomSeed, generate-structures=true, false)
//               props.createDimensions(provider) = WorldDimensionData("{}","minecraft:normal").create
//                 == WorldPresets.createNormalWorldDimensions(provider) (both: WORLD_PRESET/NORMAL
//                    .createWorldDimensions(); the FLAT branch is not taken). WorldDimensionData's
//                    ctor is not public, so the WorldPresets form is called here.
//               dims.bake(levelStems); PrimaryLevelData(settings, options, special, lifecycle)
//               new DataLoadOutput(worldData, complete.dimensionsRegistryAccess())
//     pc 173  layers.replaceFrom(WORLDGEN, worldgen, output.finalDimensions())
//     pc 200  ReloadableServerResources.loadResources(rm, layers2, pendingTags, cfg.enabledFeatures(),
//                 DEDICATED, permissions, backgroundExecutor, gameExecutor)
//     pc 210  .whenComplete(close rm on error)
//     pc 227  .thenApplyAsync(r -> { r.updateStaticRegistryTags(); WorldStem::new }, gameExecutor)
//   Pure Minecraft bytecode drives Minecraft's own reload; this file only times and folds.
//
// TIMING. RELOAD_MS = loadResources(...) + updateStaticRegistryTags + the queue drain until the
// WorldStem future is done (the task's window). REGISTRIES_MS = the WorldLoader.load steps before
// it (resource manager, tags, RegistryDataLoader worldgen + dimensions, world data). WINDOW_MS is
// their sum = all of WorldLoader.load as the dedicated server runs it. GAME_TASKS = runnables the
// main thread drained.
//
// NOT IDEMPOTENT: Bootstrap.bootStrap() runs once per process (BootstrapBench); one reload per process.
// System.out is replaced by log4j inside bootStrap(): the result line goes to the PrintStream
// captured BEFORE it. Every MC log line during the reload goes to the log4j console appender.
//
// OUTPUT (exactly one line on the original stdout; the first five fields are the contract):
//   RELOAD_MS=<ms> RECIPES=<n> ADVANCEMENTS=<n> TAGS=<n> HASH=<16 hex> REGISTRIES_MS=<ms> WINDOW_MS=<ms> GAME_TASKS=<n>
// HASH = FNV-1a 64 over a deterministic walk, every collection SORTED explicitly (the engines differ
// in HashMap order):
//   - every registry of resources.fullRegistries().lookup() (static + worldgen + dimension +
//     reloadable: loot tables, predicates, item modifiers), by registry key: element ids sorted,
//     then every tag (key sorted) with its element ids sorted  -> certifies RegistryDataLoader,
//     ReloadableServerRegistries and TagLoader/updateStaticRegistryTags;
//   - every recipe id sorted + its Recipe.CODEC JSON, canonicalised (object keys sorted recursively);
//   - every advancement id sorted + parent + criteria names sorted + requirements + Advancement.CODEC JSON;
//   - every function id and function tag sorted; every root command literal sorted.
//
// ⚠**ONE FOLDED ITEM DIVERGES BETWEEN THIS ENGINE AND THE JVM, AND IT IS A PLATFORM CAPABILITY
// RATHER THAN A SEMANTIC DIFFERENCE — so the HASH cannot be equal even when everything the
// engine computes is.** Set `RAVA_FOLD_DUMP=1` on both sides and diff (see `Fold.dump`): of
// 30,460 folded items, the JVM folds exactly one more, the root command literal `jfr`.
//
// `Commands.<init>` registers `/jfr` behind
// `JvmProfiler.INSTANCE.isAvailable()` (javap), and `JvmProfiler`'s static initialiser picks
// `JfrProfiler` only when `Runtime.class.getModule().getLayer().findModule("jdk.jfr")` is
// present AND `jdk.jfr.FlightRecorder.isAvailable()`. Measured: on `java -Xint` `INSTANCE` is
// `JfrProfiler` and `isAvailable()` is `true`; on rava it is `JvmProfiler$NoOpProfiler` and
// `false`. rava implements no Java Flight Recorder, so `false` is the CORRECT answer, and the
// command that would drive one correctly does not exist — reporting the module present would
// register a command backed by nothing.
//
// So the useful comparison for this bench is the DUMP, not the hash: every other item — each of
// the 1,470 recipe JSONs, the 1,584 advancement JSONs, and every registry id, tag and function
// name — must be byte-identical to `java -Xint`.
//
// Usage:  java -cp out;<bench\mc-cp.txt> ReloadBench
//         RAVA_FOLD_DUMP=1 ... ReloadBench   # + one `FOLD <s>` line per folded item
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Executor;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.stream.Stream;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.mojang.datafixers.util.Pair;
import com.mojang.serialization.Codec;
import com.mojang.serialization.DataResult;
import com.mojang.serialization.JsonOps;
import com.mojang.serialization.Lifecycle;

import net.minecraft.SharedConstants;
import net.minecraft.advancements.Advancement;
import net.minecraft.advancements.AdvancementHolder;
import net.minecraft.commands.Commands;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.HolderSet;
import net.minecraft.core.LayeredRegistryAccess;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.RegistryLayer;
import net.minecraft.server.ReloadableServerResources;
import net.minecraft.server.WorldLoader;
import net.minecraft.server.WorldStem;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.CloseableResourceManager;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.tags.TagLoader;
import net.minecraft.util.Util;
import net.minecraft.world.Difficulty;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.item.crafting.Recipe;
import net.minecraft.world.item.crafting.RecipeHolder;
import net.minecraft.world.level.DataPackConfig;
import net.minecraft.world.level.GameType;
import net.minecraft.world.level.LevelSettings;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.dimension.LevelStem;
import net.minecraft.world.level.gamerules.GameRules;
import net.minecraft.world.level.levelgen.WorldDimensions;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.presets.WorldPresets;
import net.minecraft.world.level.storage.PrimaryLevelData;
import net.minecraft.world.level.storage.WorldData;

public class ReloadBench {
    private static final long FNV_OFFSET = 0xcbf29ce484222325L;
    private static final long FNV_PRIME = 0x100000001b3L;

    /** Deterministic FNV-1a 64 fold. Encapsulates the hash so callers cannot mix in order-dependent noise. */
    private static final class Fold {
        private long h = FNV_OFFSET;
        private int recipes, advancements, tags;

        /**
         * `RAVA_FOLD_DUMP=1` prints every folded string, one per line, prefixed `FOLD `.
         *
         * WHY: a HASH divergence between two engines is 16 hex characters and says nothing about
         * WHICH of the ~3,000 folded items differs. The fold order is deterministic and every
         * collection inside it is explicitly sorted, so the two dumps are line-comparable and
         * `diff` names the first divergent recipe, advancement, registry id or tag directly.
         * Set on both engines, diff the two outputs.
         *
         * The hash is untouched by this — the dump reads `s` and writes nothing back — so a run
         * with the dump on produces the same HASH as a run without it.
         */
        static PrintStream dump;

        void str(String s) {
            if (dump != null) { dump.println("FOLD " + s); }
            byte[] b = s.getBytes(StandardCharsets.UTF_8);
            for (byte x : b) { h ^= (x & 0xff); h *= FNV_PRIME; }
            h ^= 0xfe; h *= FNV_PRIME; // terminator so "ab","c" != "a","bc"
        }
        void i32(int v) { for (int k = 0; k < 4; k++) { h ^= (v >>> (8 * k)) & 0xff; h *= FNV_PRIME; } }
        boolean jfrSeen = false;
        long hash() { return h; }

        /** Sorted list of strings: count, then each. */
        void sorted(List<String> xs) {
            xs.sort(null);
            i32(xs.size());
            for (String x : xs) str(x);
        }

        /** Gson JsonElement with object keys sorted recursively: immune to map-order differences. */
        static String canonical(JsonElement e) {
            StringBuilder sb = new StringBuilder();
            canonical(e, sb);
            return sb.toString();
        }
        private static void canonical(JsonElement e, StringBuilder sb) {
            if (e == null || e.isJsonNull()) { sb.append("null"); return; }
            if (e.isJsonPrimitive()) { sb.append(e.toString()); return; }
            if (e.isJsonArray()) {
                sb.append('[');
                boolean first = true;
                for (JsonElement x : (JsonArray) e) { if (!first) sb.append(','); first = false; canonical(x, sb); }
                sb.append(']');
                return;
            }
            TreeMap<String, JsonElement> m = new TreeMap<>();
            for (Map.Entry<String, JsonElement> en : ((JsonObject) e).entrySet()) m.put(en.getKey(), en.getValue());
            sb.append('{');
            boolean first = true;
            for (Map.Entry<String, JsonElement> en : m.entrySet()) {
                if (!first) sb.append(','); first = false;
                sb.append('"').append(en.getKey()).append("\":");
                canonical(en.getValue(), sb);
            }
            sb.append('}');
        }

        <T> void codec(Codec<T> codec, RegistryOps<JsonElement> ops, T value) {
            DataResult<JsonElement> r = codec.encodeStart(ops, value);
            Optional<JsonElement> j = r.result();
            if (j.isPresent()) str(canonical(j.get()));
            else str("ERR:" + r.error().map(Object::toString).orElse("?"));
        }

        /** Every registry (static, worldgen, dimension, reloadable): element ids, then tags with their elements. */
        void registries(HolderLookup.Provider provider) {
            List<HolderLookup.RegistryLookup<?>> regs = new ArrayList<>(provider.listRegistries().toList());
            regs.sort(Comparator.comparing(r -> r.key().identifier().toString()));
            i32(regs.size());
            for (HolderLookup.RegistryLookup<?> r : regs) registry(r);
        }
        private <T> void registry(HolderLookup.RegistryLookup<T> r) {
            str(r.key().identifier().toString());
            List<String> ids = new ArrayList<>();
            r.listElementIds().forEach(k -> ids.add(k.identifier().toString()));
            sorted(ids);
            List<HolderSet.Named<T>> named = new ArrayList<>(r.listTags().toList());
            named.sort(Comparator.comparing(n -> n.key().location().toString()));
            i32(named.size());
            for (HolderSet.Named<T> n : named) {
                tags++;
                str(n.key().location().toString());
                List<String> els = new ArrayList<>();
                for (Holder<T> hd : n) els.add(hd.getRegisteredName());
                sorted(els);
            }
        }

        void recipes(ReloadableServerResources res, RegistryOps<JsonElement> ops) {
            List<RecipeHolder<?>> rs = new ArrayList<>(res.getRecipeManager().getRecipes());
            rs.sort(Comparator.comparing(r -> r.id().identifier().toString()));
            i32(rs.size());
            for (RecipeHolder<?> r : rs) {
                recipes++;
                str(r.id().identifier().toString());
                codec(Recipe.CODEC, ops, r.value());
            }
        }

        void advancements(ReloadableServerResources res, RegistryOps<JsonElement> ops) {
            List<AdvancementHolder> as = new ArrayList<>(res.getAdvancements().getAllAdvancements());
            as.sort(Comparator.comparing(a -> a.id().toString()));
            i32(as.size());
            for (AdvancementHolder a : as) {
                advancements++;
                str(a.id().toString());
                Advancement v = a.value();
                str(v.parent().map(Identifier::toString).orElse(""));
                sorted(new ArrayList<>(v.criteria().keySet()));
                List<String> reqs = new ArrayList<>();
                for (List<String> group : v.requirements().requirements()) {
                    List<String> g = new ArrayList<>(group); g.sort(null); reqs.add(String.join("|", g));
                }
                sorted(reqs);
                codec(Advancement.CODEC, ops, v);
            }
        }

        void functionsAndCommands(ReloadableServerResources res) {
            List<String> fns = new ArrayList<>();
            for (Identifier id : res.getFunctionLibrary().getFunctions().keySet()) fns.add(id.toString());
            sorted(fns);
            List<String> ftags = new ArrayList<>();
            for (Identifier id : res.getFunctionLibrary().getAvailableTags()) ftags.add(id.toString());
            sorted(ftags);
            List<String> cmds = new ArrayList<>();
            for (var node : res.getCommands().getDispatcher().getRoot().getChildren()) {
                // ENGINE CAPABILITY, NOT RELOAD DATA. `Commands.<init>` registers `/jfr` only when
                // `JvmProfiler.INSTANCE.isAvailable()`, which is true on a HotSpot with the jdk.jfr
                // module and false on any engine without Flight Recorder. Folding it would make the
                // hash disagree for a reason that has nothing to do with the datapack reload -- and
                // the whole point of the hash is that a disagreement means a DEFECT. Excluded on
                // both sides, and counted so its presence is still observable.
                if (node.getName().equals("jfr")) { jfrSeen = true; continue; }
                cmds.add(node.getName());
            }
            sorted(cmds);
        }
    }

    /**
     * Util.blockUntilDone's executor, verbatim (javap -c Util.blockUntilDone(Function,Predicate)):
     * a LinkedBlockingQueue the callback adds to, drained on the calling thread with poll(100 ms)
     * until the predicate holds. This is what the dedicated server hands WorldLoader.load as its
     * game executor before a MinecraftServer exists.
     */
    private static final class GameQueue implements Executor {
        private final LinkedBlockingQueue<Runnable> queue = new LinkedBlockingQueue<>();
        private int drained;

        @Override public void execute(Runnable r) { queue.add(r); }

        <T> T drain(CompletableFuture<T> future) throws Exception {
            while (!future.isDone()) {
                Runnable r = queue.poll(100L, TimeUnit.MILLISECONDS);
                if (r != null) { r.run(); drained++; }
            }
            try {
                return future.get();
            } catch (ExecutionException error) {
                // This runs only after get() has failed, so stack inspection cannot change the schedule
                // that produced the failure.
                if (System.getenv("RAVA_TRACE_RELOAD_FAILURE") != null) {
                    for (Throwable cause = error; cause != null; cause = cause.getCause()) {
                        System.err.println("[RELOAD-FAILURE] " + cause);
                        for (StackTraceElement frame : cause.getStackTrace()) {
                            System.err.println("  at " + frame.getClassName() + "." + frame.getMethodName());
                        }
                    }
                }
                throw error;
            }
        }
        int drained() { return drained; }
    }

    /** Main.createNewWorldData(settings, ctx, levelStems, demo=false, bonusChest=false) with server.properties defaults. */
    private static WorldLoader.DataLoadOutput<WorldData> createNewWorldData(WorldLoader.DataLoadContext ctx) {
        Registry<LevelStem> levelStems = ctx.datapackDimensions().lookupOrThrow(Registries.LEVEL_STEM);
        LevelSettings levelSettings = new LevelSettings("world", GameType.SURVIVAL, false, Difficulty.EASY, false,
                new GameRules(ctx.dataConfiguration().enabledFeatures()), ctx.dataConfiguration());
        WorldOptions worldOptions = new WorldOptions(WorldOptions.randomSeed(), true, false);
        WorldDimensions dims = WorldPresets.createNormalWorldDimensions(ctx.datapackWorldgen());
        WorldDimensions.Complete complete = dims.bake(levelStems);
        Lifecycle lifecycle = complete.lifecycle().add(ctx.datapackWorldgen().allRegistriesLifecycle());
        return new WorldLoader.DataLoadOutput<>(
                new PrimaryLevelData(levelSettings, worldOptions, complete.specialWorldProperty(), lifecycle),
                complete.dimensionsRegistryAccess());
    }

    private static Fold foldResources(ReloadableServerResources res) {
        HolderLookup.Provider full = res.fullRegistries().lookup();
        RegistryOps<JsonElement> ops = RegistryOps.create(JsonOps.INSTANCE, full);
        Fold f = new Fold();
        f.registries(full);
        f.recipes(res, ops);
        f.advancements(res, ops);
        f.functionsAndCommands(res);
        return f;
    }

    private static void phase(PrintStream out, String name) {
        out.println("RELOAD_PHASE name=" + name + " epoch_ms=" + System.currentTimeMillis());
    }

    public static void main(String[] args) throws Exception {
        boolean tracePhases = false;
        for (String arg : args) if ("--trace-phases".equals(arg)) tracePhases = true;
        PrintStream realOut = System.out; // wrapStreams() replaces System.out inside bootStrap()
        // Captured BEFORE bootStrap() for the same reason the result line is: log4j replaces
        // System.out, and a dump written to the replacement is interleaved with server logging.
        if (System.getenv("RAVA_FOLD_DUMP") != null) { Fold.dump = realOut; }

        SharedConstants.tryDetectVersion(); // Main pc 0
        Bootstrap.bootStrap();               // Main pc 268 -- ONE call per process

        // Main pc 683 / 699: pack repository + InitConfig for a server without a world.
        PackRepository repo = ServerPacksSource.createVanillaTrustedRepository();
        WorldDataConfiguration cfg = new WorldDataConfiguration(DataPackConfig.DEFAULT, FeatureFlags.DEFAULT_FLAGS);
        WorldLoader.PackConfig packConfig = new WorldLoader.PackConfig(repo, cfg, false, true);
        PermissionSet permissions = LevelBasedPermissionSet.GAMEMASTER;
        WorldLoader.InitConfig initConfig = new WorldLoader.InitConfig(packConfig, Commands.CommandSelection.DEDICATED, permissions);

        GameQueue game = new GameQueue();
        Executor background = Util.backgroundExecutor();

        // ---- WorldLoader.load, pc 0-176: resource manager, tags, worldgen + dimension registries, world data.
        if (tracePhases) phase(realOut, "registries.start");
        long r0 = System.nanoTime();
        Pair<WorldDataConfiguration, CloseableResourceManager> pair = initConfig.packConfig().createResourceManager();
        CloseableResourceManager rm = pair.getSecond();
        LayeredRegistryAccess<RegistryLayer> layers = RegistryLayer.createRegistryAccess();
        List<Registry.PendingTags<?>> pendingTags = TagLoader.loadTagsForExistingRegistries(rm, layers.getLayer(RegistryLayer.STATIC));
        RegistryAccess.Frozen loadingAccess = layers.getAccessForLoading(RegistryLayer.WORLDGEN);
        List<HolderLookup.RegistryLookup<?>> lookups = TagLoader.buildUpdatedLookups(loadingAccess, pendingTags);
        RegistryAccess.Frozen worldgen = RegistryDataLoader.load(rm, lookups, RegistryDataLoader.WORLDGEN_REGISTRIES);
        List<HolderLookup.RegistryLookup<?>> lookups2 = Stream.concat(lookups.stream(), worldgen.listRegistries()).toList();
        RegistryAccess.Frozen dimensions = RegistryDataLoader.load(rm, lookups2, RegistryDataLoader.DIMENSION_REGISTRIES);
        WorldDataConfiguration dataConfig = pair.getFirst();
        HolderLookup.Provider provider = HolderLookup.Provider.create(lookups2.stream());
        WorldLoader.DataLoadOutput<WorldData> output = createNewWorldData(
                new WorldLoader.DataLoadContext(rm, dataConfig, provider, dimensions));
        LayeredRegistryAccess<RegistryLayer> layers2 = layers.replaceFrom(RegistryLayer.WORLDGEN, worldgen, output.finalDimensions());
        long r1 = System.nanoTime();

        // ---- WorldLoader.load, pc 200-227 + Util.blockUntilDone's drain: THE TIMED WINDOW.
        if (tracePhases) phase(realOut, "cold.start");
        long t0 = System.nanoTime();
        CompletableFuture<WorldStem> future = ReloadableServerResources.loadResources(
                        rm, layers2, pendingTags, dataConfig.enabledFeatures(),
                        initConfig.commandSelection(), initConfig.functionCompilationPermissions(),
                        background, game)
                .whenComplete((res, err) -> { if (err != null) rm.close(); })
                .thenApplyAsync(res -> {
                    res.updateStaticRegistryTags();
                    return new WorldStem(rm, res, layers2, output.cookie());
                }, game);
        if (tracePhases) phase(realOut, "cold.construct.end");
        WorldStem stem = game.drain(future);
        long t1 = System.nanoTime();
        if (tracePhases) phase(realOut, "cold.end");

        // ---- A SECOND RELOAD, WHICH IS THE METRIC THE A/B ACTUALLY USES.
        //
        // RELOAD_MS covers the FIRST reload this process ever runs, so it carries the whole
        // JIT warm-up of ~6,300 JSON files' worth of codec machinery. On rava that made the
        // instrument's null test (a flag that does not exist) read -3.29% min / -5.52% median
        // with a 5-9% arm spread -- useless for pricing a 2% change. A second reload over the
        // same ResourceManager is a real operation (it is what `/reload` runs) and it measures
        // the same code with the compilers warm, so the variance it carries is the workload's,
        // not the warm-up's. Both engines pay whatever caching the first pass left behind.
        // `--reloads N` runs N warm reloads instead of one and times each, so a TREND is visible.
        // It has to be: java reads 1385 then 498 ms (its compilers warmed), rava reads 18,103 then
        // 23,321 -- the REPEAT IS SLOWER. One extra pass cannot tell a one-time step (a larger live
        // set) from unbounded degradation (something accumulating per reload); three can.
        int reloads = 1;
        for (int i = 0; i < args.length; i++) {
            if ("--reloads".equals(args[i]) && i + 1 < args.length) reloads = Integer.parseInt(args[++i]);
        }
        if (reloads < 1) throw new IllegalArgumentException("reloads must be positive");
        StringBuilder more = new StringBuilder();
        List<Fold> warmResults = new ArrayList<>();
        long t2 = 0, t3 = 0;
        for (int r = 0; r < reloads; r++) {
            if (tracePhases) phase(realOut, "warm.start");
            long a = System.nanoTime();
            CompletableFuture<ReloadableServerResources> fr = ReloadableServerResources.loadResources(
                    rm, layers2, pendingTags, dataConfig.enabledFeatures(),
                    initConfig.commandSelection(), initConfig.functionCompilationPermissions(),
                    background, game);
            if (tracePhases) phase(realOut, "warm.construct.end");
            ReloadableServerResources rr = game.drain(fr);
            long b = System.nanoTime();
            if (tracePhases) phase(realOut, "warm.end");
            rr.updateStaticRegistryTags();
            if (r == 0) { t2 = a; t3 = b; }
            more.append(r == 0 ? "" : ",").append((b - a) / 1_000_000L);
            // Outside the timed interval. Keep only the digest and counts, not
            // the complete resource set, so validation does not retain old reloads.
            warmResults.add(foldResources(rr));
            if (tracePhases) phase(realOut, "warm.digest.end");
        }

        Fold f = foldResources(stem.dataPackResources());
        if (tracePhases) phase(realOut, "cold.digest.end");
        for (int r = 0; r < warmResults.size(); r++) {
            Fold warm = warmResults.get(r);
            realOut.println("WARM_RESULT index=" + r + " RECIPES=" + warm.recipes
                    + " ADVANCEMENTS=" + warm.advancements + " TAGS=" + warm.tags
                    + " HASH=" + String.format("%016x", warm.hash()));
            if (warm.hash() != f.hash() || warm.recipes != f.recipes
                    || warm.advancements != f.advancements || warm.tags != f.tags) {
                realOut.flush();
                throw new IllegalStateException("warm reload " + r + " differs from first resource set");
            }
        }

        realOut.println("RELOAD_MS=" + (t1 - t0) / 1_000_000L
                + " RELOAD2_MS=" + (t3 - t2) / 1_000_000L
                + " RELOADS=[" + more + "]"
                + " RECIPES=" + f.recipes
                + " ADVANCEMENTS=" + f.advancements
                + " TAGS=" + f.tags
                + " HASH=" + String.format("%016x", f.hash())
                + " JFR=" + f.jfrSeen
                + " REGISTRIES_MS=" + (r1 - r0) / 1_000_000L
                + " WINDOW_MS=" + (t1 - r0) / 1_000_000L
                + " GAME_TASKS=" + game.drained());
        realOut.flush();
        stem.close();
        Util.shutdownExecutors();
    }
}
