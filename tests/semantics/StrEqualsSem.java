// String.equals in compiled code (StringLatin1.equals, compared a word at a time): every length
// from 0 to 40, a difference at the first, a middle, the last and each word-boundary byte, the
// same object, and Latin-1 against UTF-16 strings of the same characters.
public class StrEqualsSem {
    static boolean eq(String a, Object b) { return a.equals(b); }

    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        long hits = 0, checks = 0;
        for (int round = 0; round < 3000; round++) {
            for (int len = 0; len <= 40; len++) {
                char[] cs = new char[len];
                for (int i = 0; i < len; i++) cs[i] = (char) ('a' + (i * 7 + len) % 26);
                String a = new String(cs), same = new String(cs);
                hits += eq(a, same) ? 1 : 0;
                hits += eq(a, a) ? 1 : 0;
                checks += 2;
                for (int pos : new int[] { 0, len / 2, len - 1, 7, 8, 15, 16, 31, 32 }) {
                    if (pos < 0 || pos >= len) continue;
                    char[] ds = cs.clone();
                    ds[pos] = (char) (ds[pos] == 'z' ? 'a' : ds[pos] + 1);
                    hits += eq(a, new String(ds)) ? 1 : 0;
                    checks++;
                }
                if (len > 0) {
                    char[] wide = cs.clone();
                    wide[len - 1] = 'ж';
                    hits += eq(a, new String(wide)) ? 1 : 0;
                    hits += eq(new String(wide), new String(wide)) ? 1 : 0;
                    checks += 2;
                }
                hits += eq(a, a + "x") ? 1 : 0;
                hits += eq(a, null) ? 1 : 0;
                hits += eq(a, (Object) cs) ? 1 : 0;
                checks += 3;
                if (round == 0 && len % 9 == 0) sb.append(len).append(':').append(hits).append(' ');
            }
        }
        System.out.println(sb);
        System.out.println("hits " + hits + " of " + checks);
    }
}
