// The exact RandomState nesting, registry-free: base(seed).forkPositional()
// .fromHashOf("minecraft:temperature") -> NormalNoise.create(TEMP params).
// JVM registry run shows first-octave xo=130.444; if the JVM probe reproduces
// that and rava's differs, the fork-of-fork hop (or NormalNoise doubling) is
// the divergence; if both match JVM, the registry string/params are at fault.
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

public class NestedFork {
    public static void main(String[] a) {
        XoroshiroRandomSource base = new XoroshiroRandomSource(1234567L);
        RandomSource rnd2 = base.forkPositional().fromHashOf("minecraft:temperature");
        NormalNoise nn = NormalNoise.create(
            rnd2, new NormalNoise.NoiseParameters(-10, 1.5, 0.0, 1.0, 0.0, 0.0, 0.0));
        StringBuilder sb = new StringBuilder();
        nn.parityConfigString(sb);
        System.out.println("TEMP_NESTED=" + sb);
    }
}
