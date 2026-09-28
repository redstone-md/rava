// Isolates the per-character guest string-scan cost — the shape that dominates every
// JSON/codec/toString path (gson's tokenizer, DFU's toString map keys, the datapack
// hash fold). No Minecraft classes, so it runs in ~1s and can be A/B'd freely.
//
// Three loops over the same data, each adding one operation to the previous:
//   LEN     hoisted length(), no char access  — loop + arithmetic floor
//   CHARAT  s.charAt(i) per iteration         — adds the char-access bridge
//   LENCALL s.length() re-called per iteration — adds the length bridge (what a naive
//                                                `for (i=0; i<s.length(); i++)` costs)
// Checksums are printed so a native fast-path can be proven bit-exact.
public class StrScan {
    public static void main(String[] args) {
        int reps = args.length > 0 ? Integer.parseInt(args[0]) : 200;
        // arg 1 = string length in "records"; short strings isolate the per-CALL bridge
        // tax, long ones expose any O(n)/O(i) scan inside the accessor.
        int recs = args.length > 1 ? Integer.parseInt(args[1]) : 2000;
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < recs; i++) sb.append("{\"key").append(i).append("\":\"value\"},");
        String s = sb.toString();
        System.out.println("LEN=" + s.length() + " reps=" + reps + " chars=" + ((long) s.length() * reps));

        // warm
        for (int r = 0; r < 5; r++) { lenOnly(s); charAt(s); lenCall(s); }

        long t = System.nanoTime();
        long h1 = 0; for (int r = 0; r < reps; r++) h1 += lenOnly(s);
        long lenMs = (System.nanoTime() - t) / 1_000_000L;

        t = System.nanoTime();
        long h2 = 0; for (int r = 0; r < reps; r++) h2 += charAt(s);
        long charMs = (System.nanoTime() - t) / 1_000_000L;

        t = System.nanoTime();
        long h3 = 0; for (int r = 0; r < reps; r++) h3 += lenCall(s);
        long lenCallMs = (System.nanoTime() - t) / 1_000_000L;

        long n = (long) s.length() * reps;
        System.out.println("STRSCAN LEN_MS=" + lenMs + " CHARAT_MS=" + charMs
            + " LENCALL_MS=" + lenCallMs
            + " ps_per_char_charAt=" + ((charMs - lenMs) * 1000000000L / n)
            + " ps_per_char_lencall=" + ((lenCallMs - charMs) * 1000000000L / n)
            + " H=" + h1 + "," + h2 + "," + h3);
    }

    static long lenOnly(String s) {
        long h = 1469598103934665603L;
        int n = s.length();
        for (int i = 0; i < n; i++) { h ^= (i & 0xFF); h *= 1099511628211L; }
        return h;
    }

    static long charAt(String s) {
        long h = 1469598103934665603L;
        int n = s.length();
        for (int i = 0; i < n; i++) { h ^= (s.charAt(i) & 0xFF); h *= 1099511628211L; }
        return h;
    }

    static long lenCall(String s) {
        long h = 1469598103934665603L;
        for (int i = 0; i < s.length(); i++) { h ^= (s.charAt(i) & 0xFF); h *= 1099511628211L; }
        return h;
    }
}
