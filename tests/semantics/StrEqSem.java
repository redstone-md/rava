// String.equals of Latin-1 strings in compiled code: every length up to 20, a difference at
// every position (first byte, last byte, past a word), the same contents in distinct strings,
// one string with itself, and lengths that differ.
public class StrEqSem {
    static boolean eq(String a, String b) { return a.equals(b); }

    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        String[] base = new String[21];
        for (int n = 0; n <= 20; n++) {
            base[n] = sb.toString();
            sb.append((char) ('a' + n % 26));
        }
        long sum = 0;
        int[] counts = new int[4];
        for (int round = 0; round < 3000; round++) {
            for (int n = 0; n <= 20; n++) {
                String a = base[n];
                String same = new String(a.toCharArray());
                if (eq(a, same)) { sum += n; counts[0]++; }
                if (eq(a, a)) counts[1]++;
                for (int i = 0; i < n; i++) {
                    char[] c = a.toCharArray();
                    c[i] = (char) (c[i] ^ 1);
                    if (eq(a, new String(c))) { sum += 1000; counts[2]++; }
                }
                if (n < 20 && eq(a, base[n + 1])) counts[3]++;
            }
        }
        System.out.println("sum=" + sum + " equal=" + counts[0] + " self=" + counts[1] + " differ=" + counts[2] + " longer=" + counts[3]);
    }
}
