// ArenaString → hashing/encoding probe (no bootstrap; JIT warms the concat so
// rava builds ArenaStrings): does md5/getBytes of a CONCAT-BUILT string match
// the literal? The path_str audit class — natives matching only Value::Str
// silently drop ArenaString content.
import com.google.common.hash.Hashing;
import java.nio.charset.StandardCharsets;

public class HashStr {
    public static void main(String[] args) throws Exception {
        String lit = "minecraft:temperature";
        long sink = 0;
        String built = null;
        for (int i = 0; i < 200_000; i++) {
            built = "minecraft" + ":" + "temperature".substring(0);
            sink += built.length();
        }
        System.out.println("SINK=" + sink + " EQ=" + lit.equals(built));
        byte[] litB = lit.getBytes(StandardCharsets.UTF_8);
        byte[] bltB = built.getBytes(StandardCharsets.UTF_8);
        System.out.println("LITLEN=" + litB.length + " BLTLEN=" + bltB.length);
        System.out.println("LIT_MD5=" + hex(Hashing.md5().hashString(lit, StandardCharsets.UTF_8).asBytes()));
        System.out.println("BLT_MD5=" + hex(Hashing.md5().hashString(built, StandardCharsets.UTF_8).asBytes()));
        System.out.println("BLT_UNHASHED=" + hex(bltB));
    }

    static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(Character.forDigit((x >> 4) & 0xF, 16)).append(Character.forDigit(x & 0xF, 16));
        return sb.toString();
    }
}
