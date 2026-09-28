// No-bootstrap probe for the noise-seed hash chain: JDK MessageDigest MD5 vs
// guava Hashing.md5().hashString — the seeding path XoroshiroRandomSource
// .fromHashOf uses for EVERY named noise. Runs in seconds on rava.
import java.security.MessageDigest;
import com.google.common.hash.Hashing;
import java.nio.charset.StandardCharsets;

public class Md5Probe {
    public static void main(String[] args) throws Exception {
        String s = "minecraft:temperature";
        MessageDigest md = MessageDigest.getInstance("MD5");
        System.out.println("ALGO=" + md.getAlgorithm() + " LEN=" + md.getDigestLength());
        System.out.println("JDK=" + hex(md.digest(s.getBytes(StandardCharsets.UTF_8))));
        System.out.println("GUAVA=" + hex(Hashing.md5().hashString(s, StandardCharsets.UTF_8).asBytes()));
        // Concat-built algorithm name (ArenaString under JIT) — the audit-class trap.
        String alg = "MD" + (args.length + 5);
        MessageDigest md2 = MessageDigest.getInstance(alg);
        System.out.println("CONCAT_ALGO=" + md2.getAlgorithm() + " LEN=" + md2.getDigestLength());
    }

    static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02x", x));
        return sb.toString();
    }
}
