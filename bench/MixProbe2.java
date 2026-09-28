// Seed128bit.xor/mixed cold-vs-warm: the fromHashOf chain is
// seedFromHashOf(s).xor(seedLo, seedHi) -> new XoroshiroRandomSource(lo, hi).
// MixProbe cleared the static entry points; this hits the record methods
// (wide args THROUGH a record receiver) hot enough to compile.
import net.minecraft.world.level.levelgen.RandomSupport;

public class MixProbe2 {
    public static void main(String[] a) {
        RandomSupport.Seed128bit base = RandomSupport.seedFromHashOf("minecraft:temperature");
        RandomSupport.Seed128bit x0 = base.xor(0x123456789abcdefL, 0x7edcba9876543210L);
        RandomSupport.Seed128bit m0 = x0.mixed();
        RandomSupport.Seed128bit s0 = base.xor(RandomSupport.upgradeSeedTo128bit(42L));
        long xlo = 0, xhi = 0, mlo = 0, mhi = 0, slo = 0, shi = 0;
        for (int i = 0; i < 60000; i++) {
            RandomSupport.Seed128bit x = base.xor(0x123456789abcdefL, 0x7edcba9876543210L);
            xlo = x.seedLo(); xhi = x.seedHi();
            RandomSupport.Seed128bit m = x.mixed();
            mlo = m.seedLo(); mhi = m.seedHi();
            RandomSupport.Seed128bit s = base.xor(RandomSupport.upgradeSeedTo128bit(42L));
            slo = s.seedLo(); shi = s.seedHi();
        }
        p("XOR2", x0.seedLo(), x0.seedHi(), xlo, xhi);
        p("MIXD", m0.seedLo(), m0.seedHi(), mlo, mhi);
        p("XORS", s0.seedLo(), s0.seedHi(), slo, shi);
    }

    static void p(String n, long clo, long chi, long wlo, long whi) {
        System.out.println(n + " cold=" + Long.toHexString(clo) + "," + Long.toHexString(chi)
            + " warm=" + Long.toHexString(wlo) + "," + Long.toHexString(whi)
            + ((clo == wlo && chi == whi) ? " OK" : " DIVERGED"));
    }
}
