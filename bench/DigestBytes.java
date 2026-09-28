// baload on a NATIVE-created byte[] (MessageDigest.digest), cold vs warm.
// PureBytes (bytecode-created array) is clean; if this diverges, the compiled
// baload path misreads arrays allocated by Rust natives (layout mismatch) —
// the RandomSupport.seedFromHashOf divergence root, MC-free.
import java.security.MessageDigest;
import java.nio.charset.StandardCharsets;

public class DigestBytes {
    static long pair(byte[] a) {
        long lo = 0, hi = 0;
        for (int i = 0; i < 8; i++) lo = lo << 8 | (a[i] & 0xFFL);
        for (int i = 8; i < 16; i++) hi = hi << 8 | (a[i] & 0xFFL);
        return lo * 31 + hi;
    }

    public static void main(String[] s) throws Exception {
        MessageDigest md = MessageDigest.getInstance("MD5");
        byte[] a0 = md.digest("minecraft:temperature".getBytes(StandardCharsets.UTF_8));
        long cold = pair(a0);
        long warm = 0;
        for (int i = 0; i < 60000; i++) {
            byte[] a = md.digest("minecraft:temperature".getBytes(StandardCharsets.UTF_8));
            warm = pair(a);
        }
        System.out.println("COLD=" + Long.toHexString(cold) + " WARM=" + Long.toHexString(warm)
            + (cold == warm ? " OK" : " DIVERGED"));
    }
}
