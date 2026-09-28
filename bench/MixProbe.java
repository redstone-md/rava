// RandomSupport self-repro: cold (interpreted) vs warm (JIT-compiled) values
// of every RandomSupport entry point. A warm value differing from its own cold
// value = the exact mis-compiled method (StateSeed divergence root).
import net.minecraft.world.level.levelgen.RandomSupport;

public class MixProbe {
    public static void main(String[] a) {
        long m0 = RandomSupport.mixStafford13(1234567L);
        RandomSupport.Seed128bit u0 = RandomSupport.upgradeSeedTo128bit(1234567L);
        RandomSupport.Seed128bit r0 = RandomSupport.upgradeSeedTo128bitUnmixed(1234567L);
        RandomSupport.Seed128bit h0 = RandomSupport.seedFromHashOf("minecraft:temperature");
        long m = 0, ulo = 0, uhi = 0, rlo = 0, rhi = 0, hlo = 0, hhi = 0;
        for (int i = 0; i < 60000; i++) {
            m = RandomSupport.mixStafford13(1234567L);
            RandomSupport.Seed128bit u = RandomSupport.upgradeSeedTo128bit(1234567L);
            ulo = u.seedLo(); uhi = u.seedHi();
            RandomSupport.Seed128bit r = RandomSupport.upgradeSeedTo128bitUnmixed(1234567L);
            rlo = r.seedLo(); rhi = r.seedHi();
            if ((i & 8191) == 0) {
                RandomSupport.Seed128bit h = RandomSupport.seedFromHashOf("minecraft:temperature");
                hlo = h.seedLo(); hhi = h.seedHi();
            }
        }
        System.out.println("MIX  cold=" + Long.toHexString(m0) + " warm=" + Long.toHexString(m)
            + (m0 == m ? " OK" : " DIVERGED"));
        System.out.println("UPG  cold=" + Long.toHexString(u0.seedLo()) + "," + Long.toHexString(u0.seedHi())
            + " warm=" + Long.toHexString(ulo) + "," + Long.toHexString(uhi)
            + ((u0.seedLo() == ulo && u0.seedHi() == uhi) ? " OK" : " DIVERGED"));
        System.out.println("UNMX cold=" + Long.toHexString(r0.seedLo()) + "," + Long.toHexString(r0.seedHi())
            + " warm=" + Long.toHexString(rlo) + "," + Long.toHexString(rhi)
            + ((r0.seedLo() == rlo && r0.seedHi() == rhi) ? " OK" : " DIVERGED"));
        System.out.println("HASH cold=" + Long.toHexString(h0.seedLo()) + "," + Long.toHexString(h0.seedHi())
            + " warm=" + Long.toHexString(hlo) + "," + Long.toHexString(hhi)
            + ((h0.seedLo() == hlo && h0.seedHi() == hhi) ? " OK" : " DIVERGED"));
    }
}
