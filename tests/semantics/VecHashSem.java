import java.util.Arrays;

// ArraysSupport.vectorizedHashCode (String.hashCode, Arrays.hashCode) over every length from 0
// to 100 and every element kind, with negative and high values, from offsets inside the array:
// the vector blocks of 32 and 8 and the scalar tail all meet.
public class VecHashSem {
    public static void main(String[] args) {
        long acc = 0;
        java.util.Random r = new java.util.Random(42);
        for (int round = 0; round < 200; round++) {
            for (int n = 0; n <= 100; n++) {
                byte[] b = new byte[n]; char[] c = new char[n]; short[] s = new short[n]; int[] i = new int[n]; boolean[] z = new boolean[n];
                for (int k = 0; k < n; k++) { int x = r.nextInt(); b[k] = (byte) x; c[k] = (char) (x >>> 8); s[k] = (short) (x >> 3); i[k] = x; z[k] = (x & 1) != 0; }
                acc = acc * 31 + Arrays.hashCode(b) + 3L * Arrays.hashCode(c) + 5L * Arrays.hashCode(s) + 7L * Arrays.hashCode(i) + 11L * Arrays.hashCode(z);
                StringBuilder latin = new StringBuilder(), utf = new StringBuilder();
                for (int k = 0; k < n; k++) { latin.append((char) (32 + (b[k] & 0xff) % 200)); utf.append((char) (0x100 + c[k] % 5000)); }
                acc += latin.toString().hashCode() * 13L + utf.toString().hashCode() * 17L;
                if (n > 4) acc += new String(c, 2, n - 3).hashCode() + new String(b, 1, n - 2, java.nio.charset.StandardCharsets.ISO_8859_1).hashCode();
            }
        }
        System.out.println("acc " + acc);
    }
}
