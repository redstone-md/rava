// PerlinNoise octave-seeding hop: each octave's ImprovedNoise comes from
// forkPositional().fromHashOf("octave_" + k). SeedChain proved the random
// stream + fromHashOf(temperature) bit-exact, yet the octave origins diverge —
// so the bug is here: the concat string, its hash, or ImprovedNoise init.
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.synth.ImprovedNoise;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

public class OctaveProbe {
    public static void main(String[] a) {
        int k = -10;
        System.out.println("CONCAT=[octave_" + k + "]");
        XoroshiroRandomSource r = new XoroshiroRandomSource(1234567L);
        var pf = r.forkPositional();
        for (String n : new String[] { "octave_-10", "octave_-8", "octave_0" }) {
            RandomSource src = pf.fromHashOf(n);
            ImprovedNoise in = new ImprovedNoise(src);
            StringBuilder sb = new StringBuilder();
            in.parityConfigString(sb);
            System.out.println(n + " -> " + sb);
        }
        NormalNoise nn = NormalNoise.create(
            new XoroshiroRandomSource(55L),
            new NormalNoise.NoiseParameters(-10, 1.5, 0.0, 1.0, 0.0, 0.0, 0.0));
        StringBuilder sb = new StringBuilder();
        nn.parityConfigString(sb);
        System.out.println("NN=" + sb);
    }
}
