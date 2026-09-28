// The SETUP layer of the datapack probes, alone, so it can be profiled.
//
// DataPackPhases reports SETUP = 19,442 ms against java's 3,813 (5.1x) for two calls:
// SharedConstants.tryDetectVersion() and Bootstrap.bootStrap(). That is the largest single
// chunk of that probe and it is real boot work -- the server boot pays it too. Everything else
// in DataPackPhases runs after it and would otherwise dominate a profile of the whole probe.
//
// Deterministic and side-effect-free: it registers the vanilla registries and exits. No world,
// no port, no datapack walk.
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;

public class BootstrapOnly {
    public static void main(String[] args) {
        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        long t1 = System.nanoTime();
        Bootstrap.bootStrap();
        long t2 = System.nanoTime();
        Bootstrap.validate();
        long t3 = System.nanoTime();
        System.out.printf("BOOTSTRAP DETECT_MS=%d BOOTSTRAP_MS=%d VALIDATE_MS=%d TOTAL_MS=%d%n",
                (t1 - t0) / 1_000_000L, (t2 - t1) / 1_000_000L, (t3 - t2) / 1_000_000L,
                (t3 - t0) / 1_000_000L);
    }
}
