// Values spilled because a rare path of a loop (a call, many temporaries) needs registers, read
// in the loop through copies made before it; copies of copies taken back together when they get
// no register. A regular expression over format strings exercises nested inlined matchers.
public class LoopCopySem {
    static int rare(int a, int b, int c, int d, int e, int f) { return a ^ b ^ c ^ d ^ e ^ f; }

    static long scan(String s, int[] w, int k) {
        long sum = 0;
        int a = k * 3, b = k * 5, c = k * 7, d = k * 11, e = k * 13;
        for (int i = 0; i < s.length(); i++) {
            char ch = s.charAt(i);
            if (ch == '%') sum += rare(a, b, c, d, e, i) + w[i % w.length];
            else sum += ch + a + w[(i + b) % w.length];
        }
        return sum;
    }

    public static void main(String[] args) {
        java.util.regex.Pattern p = java.util.regex.Pattern.compile("%(\\d+\\$)?([-#+ 0,(\\<]*)?(\\d+)?(\\.\\d+)?([tT])?([a-zA-Z%])");
        String[] fs = {"XYZ: %.3f / %.5f", "Block: %d %d [%d]", "%-10s|%08.2f|%,d", "plain text without any"};
        int[] w = {3, 1, 4, 1, 5, 9, 2, 6};
        long h = 0;
        for (int r = 0; r < 60000; r++) {
            String s = fs[r % fs.length];
            java.util.regex.Matcher m = p.matcher(s);
            int at = 0;
            while (m.find(at)) { h = h * 31 + m.start() * 7 + m.end(); at = m.end(); }
            h += scan(s, w, r & 15);
        }
        System.out.println(h);
    }
}
