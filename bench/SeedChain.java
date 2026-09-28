// The noise-seed chain, step by step: RandomSupport.seedFromHashOf (MD5 →
// two big-endian longs), XoroshiroRandomSource(seed) upgrade, forkPositional
// fromHashOf, and the raw Xoroshiro128++ stream. Diff JVM vs rava to pin the
// exact hop where every named noise's origins diverge (ClimateVals/RingScan).
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;

public class SeedChain {
    public static void main(String[] a) {
        RandomSupport.Seed128bit s = RandomSupport.seedFromHashOf("minecraft:temperature");
        System.out.println("LO=" + Long.toHexString(s.seedLo()) + " HI=" + Long.toHexString(s.seedHi()));
        XoroshiroRandomSource r = new XoroshiroRandomSource(1234567L);
        RandomSource pos = r.forkPositional().fromHashOf("minecraft:temperature");
        for (int i = 0; i < 4; i++) System.out.println("NL" + i + "=" + Long.toHexString(pos.nextLong()));
        XoroshiroRandomSource x = new XoroshiroRandomSource(11L, 22L);
        for (int i = 0; i < 4; i++) System.out.println("XR" + i + "=" + Long.toHexString(x.nextLong()));
        XoroshiroRandomSource u = new XoroshiroRandomSource(1234567L);
        System.out.println("U0=" + Long.toHexString(u.nextLong()));
        System.out.println("D0=" + new XoroshiroRandomSource(99L).nextDouble());
    }
}
