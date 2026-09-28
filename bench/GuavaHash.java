// Guava Hashing.md5().hashString(...).asBytes() cold vs warm — the exact
// producer RandomSupport.seedFromHashOf uses. JDK MessageDigest path is clean;
// if this diverges the bug is in the compiled guava chain (hashString/asBytes).
import com.google.common.hash.Hashing;
import java.nio.charset.StandardCharsets;

public class GuavaHash {
    static long pair(byte[] a) {
        long lo = 0, hi = 0;
        for (int i = 0; i < 8; i++) lo = lo << 8 | (a[i] & 0xFFL);
        for (int i = 8; i < 16; i++) hi = hi << 8 | (a[i] & 0xFFL);
        return lo * 31 + hi;
    }

    public static void main(String[] s) {
        byte[] a0 = Hashing.md5().hashString("minecraft:temperature", StandardCharsets.UTF_8).asBytes();
        long cold = pair(a0);
        long warm = 0;
        for (int i = 0; i < 60000; i++) {
            byte[] a = Hashing.md5().hashString("minecraft:temperature", StandardCharsets.UTF_8).asBytes();
            warm = pair(a);
        }
        System.out.println("COLD=" + Long.toHexString(cold) + " WARM=" + Long.toHexString(warm)
            + (cold == warm ? " OK" : " DIVERGED"));
    }
}
