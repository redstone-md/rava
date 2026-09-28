// seedFromHashOf with ALTERNATING strings, cold vs warm: a compiled caller
// corrupting the (possibly shared/reset) MessageDigest state shows up as a
// warm hash depending on the PREVIOUS call's string — invisible to probes
// that hash one constant string.
import net.minecraft.world.level.levelgen.RandomSupport;

public class HashAlt {
    public static void main(String[] a) {
        String[] names = { "minecraft:temperature", "minecraft:vegetation",
            "minecraft:continentalness", "octave_-10", "octave_0", "x" };
        long[] clo = new long[names.length], chi = new long[names.length];
        for (int j = 0; j < names.length; j++) {
            RandomSupport.Seed128bit s = RandomSupport.seedFromHashOf(names[j]);
            clo[j] = s.seedLo(); chi[j] = s.seedHi();
        }
        long[] wlo = new long[names.length], whi = new long[names.length];
        for (int i = 0; i < 30000; i++) {
            for (int j = 0; j < names.length; j++) {
                RandomSupport.Seed128bit s = RandomSupport.seedFromHashOf(names[j]);
                wlo[j] = s.seedLo(); whi[j] = s.seedHi();
            }
        }
        boolean ok = true;
        for (int j = 0; j < names.length; j++) {
            boolean same = clo[j] == wlo[j] && chi[j] == whi[j];
            if (!same) ok = false;
            System.out.println(names[j] + " cold=" + Long.toHexString(clo[j]) + ","
                + Long.toHexString(chi[j]) + " warm=" + Long.toHexString(wlo[j]) + ","
                + Long.toHexString(whi[j]) + (same ? " OK" : " DIVERGED"));
        }
        System.out.println(ok ? "ALL_OK" : "DIVERGENCE");
    }
}
