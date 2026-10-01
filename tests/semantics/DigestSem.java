import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.Random;

/** Message digests over many lengths and chunkings (single and multi-block updates). */
public class DigestSem {
    public static void main(String[] args) throws Exception {
        Random r = new Random(7);
        byte[] data = new byte[1 << 20];
        r.nextBytes(data);
        for (String alg : new String[] {"SHA-1", "SHA-224", "SHA-256", "SHA-384", "SHA-512", "SHA-512/256", "MD5"}) {
            MessageDigest md = MessageDigest.getInstance(alg);
            StringBuilder all = new StringBuilder();
            for (int len : new int[] {0, 1, 55, 56, 63, 64, 65, 111, 112, 127, 128, 129, 1000, 65536, data.length}) {
                md.update(data, 0, len);
                all.append(HexFormat.of().formatHex(md.digest()), 0, 8).append(' ');
            }
            // Uneven chunks through the multi-block path.
            for (int off = 0, step = 1; off < data.length; off += step, step = step * 3 % 4099 + 1) {
                md.update(data, off, Math.min(step, data.length - off));
            }
            all.append(HexFormat.of().formatHex(md.digest()));
            System.out.println(alg + " " + all);
        }
        long t0 = System.nanoTime();
        MessageDigest md = MessageDigest.getInstance("SHA-256");
        for (int i = 0; i < 64; i++) md.update(data);
        System.out.println("64 MiB sha-256 " + HexFormat.of().formatHex(md.digest()).substring(0, 16));
        System.err.println("time " + (System.nanoTime() - t0) / 1_000_000 + " ms");
    }
}
