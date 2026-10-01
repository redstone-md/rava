import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.synth.ImprovedNoise;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

/** Steady-state speed of Minecraft's world-generation noise (the classes of the Paper server
 *  jar, on its classpath): ns per call of ImprovedNoise.noise and NormalNoise.getValue, after
 *  warm-up, with a checksum to compare VMs. Run with `-cp <paper jar>:<libraries>`. */
public class McNoiseBench {
    interface Kernel { double run(int i); }

    static double time(String name, Kernel k, int n) {
        double sum = 0;
        for (int w = 0; w < 5; w++) for (int i = 0; i < n; i++) sum += k.run(i);
        long t0 = System.nanoTime();
        for (int r = 0; r < 5; r++) for (int i = 0; i < n; i++) sum += k.run(i);
        double ns = (System.nanoTime() - t0) / (5.0 * n);
        System.out.printf("%-14s %8.1f ns/op%n", name, ns);
        return sum;
    }

    public static void main(String[] args) {
        RandomSource rs = new XoroshiroRandomSource(42);
        ImprovedNoise improved = new ImprovedNoise(rs);
        NormalNoise normal = NormalNoise.create(rs, -7, 1.0, 1.0, 0.0, 0.5);
        NormalNoise deep = NormalNoise.create(rs, -10, 1.5, 0.0, 1.0, 0.0, 0.0, 0.0);
        double check = 0;
        check += time("improved", i -> improved.noise(i * 0.37, (i & 255) * 0.11, i * 0.013), 2_000_000);
        check += time("normal", i -> normal.getValue(i * 0.37, (i & 255) * 0.11, i * 0.013), 500_000);
        check += time("deep", i -> deep.getValue(i * 1.7, (i & 63) * 0.5, i * 0.29), 500_000);
        System.out.println("check " + check);
    }
}
