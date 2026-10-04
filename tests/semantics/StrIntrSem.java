// String and array routines optimized code runs in place of the JDK's intrinsic candidates
// (StringLatin1.equals, ArraysSupport.vectorizedMismatch / vectorizedHashCode,
// StringCoding.countPositives): the same results as the bytecode, hot and cold, for Latin-1 and
// UTF-16 strings, every array kind, all lengths around the word size and mismatches anywhere.
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

public class StrIntrSem {
    static long check(int n, int at) {
        StringBuilder a = new StringBuilder(), u = new StringBuilder();
        for (int i = 0; i < n; i++) { a.append((char) ('a' + (i * 7) % 26)); u.append((char) (0x400 + i % 50)); }
        String s = a.toString(), t = a.toString(), us = u.toString();
        String d = n == 0 ? s : s.substring(0, at) + 'Z' + s.substring(at + 1);
        long h = s.hashCode() * 31L + us.hashCode();
        h = h * 31 + (s.equals(t) ? 1 : 0) + (s.equals(d) ? 2 : 0) + (us.equals(s) ? 4 : 0);
        h = h * 31 + Integer.signum(s.compareTo(d)) + 3 * Integer.signum(d.compareTo(s + "x"));
        byte[] b = s.getBytes(StandardCharsets.ISO_8859_1), c = b.clone();
        if (n > 0) c[at] ^= 1;
        h = h * 31 + Arrays.mismatch(b, c) + Arrays.hashCode(b) + (Arrays.equals(b, c) ? 1 : 0);
        int[] ia = new int[n], ib = new int[n]; long[] la = new long[n], lb = new long[n];
        char[] ca = s.toCharArray(), cb = d.toCharArray(); short[] sa = new short[n];
        for (int i = 0; i < n; i++) { ia[i] = ib[i] = i * 31 - 7; la[i] = lb[i] = (long) i << 40; sa[i] = (short) (i * 1000); }
        if (n > 0) { ib[at] = -1; lb[at] = 1; }
        h = h * 31 + Arrays.mismatch(ia, ib) + Arrays.mismatch(la, lb) + Arrays.mismatch(ca, cb);
        h = h * 31 + Arrays.hashCode(ia) + Arrays.hashCode(ca) + Arrays.hashCode(sa) + Arrays.hashCode(la);
        byte[] mixed = (s + "é€" + s).getBytes(StandardCharsets.UTF_8);
        h = h * 31 + new String(mixed, StandardCharsets.UTF_8).hashCode() + new String(mixed, StandardCharsets.ISO_8859_1).length();
        return h;
    }

    public static void main(String[] args) {
        long total = 0;
        for (int rep = 0; rep < 3000; rep++)
            for (int n = 0; n < 40; n++) total = total * 1000003 + check(n, n == 0 ? 0 : (rep * 13) % n);
        System.out.println("total " + total);
        for (int n : new int[] {0, 1, 7, 8, 9, 15, 16, 17, 33}) System.out.println(n + " " + check(n, n / 2));
    }
}
