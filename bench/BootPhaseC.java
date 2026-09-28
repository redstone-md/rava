// Replays net.minecraft.server.Main.main step by step, in bytecode order, from
// Bootstrap.bootStrap() up to the datapack load — WITHOUT binding a server port.
// The real boot's log shows a 19-26s gap between "Environment:" (logged by the
// YggdrasilAuthenticationService ctor, Main pc 416) and "Failed to request yggdrasil
// public key" (logged inside Services.create, Main pc 421), and nothing has ever
// attributed it. Each step prints its own ms, so the gap is placed on a real call
// rather than inferred from log-line adjacency.
//
// Order below matches `javap -c net.minecraft.server.Main`:
//   243 CrashReport.preload           268 Bootstrap.bootStrap     271 Bootstrap.validate
//   274 Util.startTimerHackThread     294 DedicatedServerSettings 312 RegionFileVersion.configure
//   416 new YggdrasilAuthenticationService                        421 Services.create
//   460 LevelStorageSource.createDefault  469 validateAndCreateAccess  484 getDataTag
//   493 getSummary                    683 ServerPacksSource.createPackRepository
import java.io.File;
import java.net.Proxy;
import java.nio.file.Path;
import java.nio.file.Paths;
import com.mojang.authlib.yggdrasil.YggdrasilAuthenticationService;
import net.minecraft.CrashReport;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.Services;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.util.Util;
import net.minecraft.world.level.chunk.storage.RegionFileVersion;
import net.minecraft.world.level.storage.LevelStorageSource;

public class BootPhaseC {
    static long t;
    static void step(String name) {
        long now = System.nanoTime();
        System.out.println("STEP " + name + " ms=" + ((now - t) / 1_000_000L));
        System.out.flush();
        t = now;
    }

    public static void main(String[] args) throws Exception {
        t = System.nanoTime();
        SharedConstants.tryDetectVersion();               step("tryDetectVersion");
        CrashReport.preload();                            step("CrashReport.preload");
        Bootstrap.bootStrap();                            step("Bootstrap.bootStrap");
        Bootstrap.validate();                             step("Bootstrap.validate");
        Util.startTimerHackThread();                      step("startTimerHackThread");

        Path props = Paths.get("server.properties");
        var settings = new net.minecraft.server.dedicated.DedicatedServerSettings(props);
        step("DedicatedServerSettings");
        RegionFileVersion.configure(settings.getProperties().regionFileComression);
        step("RegionFileVersion.configure");

        YggdrasilAuthenticationService auth = new YggdrasilAuthenticationService(Proxy.NO_PROXY);
        step("new YggdrasilAuthenticationService  <-- 'Environment:' is logged here");
        Services services = Services.create(auth, new File("usercache.json"));
        step("Services.create  <-- 'Failed to request yggdrasil public key' is logged here");

        LevelStorageSource lss = LevelStorageSource.createDefault(Paths.get("."));
        step("LevelStorageSource.createDefault");
        LevelStorageSource.LevelStorageAccess access = lss.validateAndCreateAccess("world");
        step("validateAndCreateAccess");
        System.out.println("hasWorldData=" + access.hasWorldData());
        var tag = access.getDataTag();                    step("getDataTag");
        var summary = access.getSummary(tag);             step("getSummary");
        PackRepository repo = ServerPacksSource.createPackRepository(access);
        step("ServerPacksSource.createPackRepository");
        repo.reload();                                    step("PackRepository.reload");
        System.out.println("PACKS available=" + repo.getAvailableIds().size()
            + " selected=" + repo.getSelectedIds().size()
            + " services=" + (services == null ? "null" : "ok")
            + " summary=" + (summary == null ? "null" : summary.getLevelName()));

        // --- the actual datapack / registry / recipe load (Main pc 721) -------
        // This is block C proper: WorldLoader.load builds the resource manager over the
        // pack repository, runs RegistryDataLoader over every /data JSON through its DFU
        // codec, and reloads ReloadableServerResources (tags, loot, advancements, recipes
        // — the "Loaded 1470 recipes" line comes from in here).
        var dataConfig = LevelStorageSource.readDataConfig(tag);
        step("readDataConfig");
        var packConfig = new net.minecraft.server.WorldLoader.PackConfig(repo, dataConfig, false, false);
        var initConfig = new net.minecraft.server.WorldLoader.InitConfig(
            packConfig,
            net.minecraft.commands.Commands.CommandSelection.DEDICATED,
            net.minecraft.server.permissions.PermissionSet.ALL_PERMISSIONS);
        step("InitConfig");

        java.util.concurrent.Executor sync = Runnable::run;
        System.getProperty("rava.wall.reset");
        System.getProperty("rava.bridge.reset");
        var future = net.minecraft.server.WorldLoader.load(
            initConfig,
            ctx -> {
                // Mirror Main's lambda: bake the dimensions the server.properties ask for.
                var dims = settings.getProperties().createDimensions(ctx.datapackWorldgen());
                var complete = dims.bake(ctx.datapackDimensions()
                    .lookupOrThrow(net.minecraft.core.registries.Registries.LEVEL_STEM));
                return new net.minecraft.server.WorldLoader.DataLoadOutput<>(
                    complete.specialWorldProperty(), complete.dimensionsRegistryAccess());
            },
            (resources, dataPackResources, registries, cookie) -> {
                resources.close();
                return "ok";
            },
            sync, sync);
        Object stem = future.get();
        step("WorldLoader.load  <-- datapack + registries + recipes (BLOCK C)");
        System.getProperty("rava.wall.dump.BLOCKC");
        System.getProperty("rava.bridge.dump.BLOCKC");
        System.out.println("STEM=" + stem);
        access.close();
    }
}
