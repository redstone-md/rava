// Which String REPRESENTATION reaches a compiled charAt? The arena-string fast arm
// only fires for a Value::Obj receiver; a boxed Value::Str takes the callback. Same
// loop over four provenances, so the per-char cost identifies the representation.
public class StrRepr {
    static final String LDC = "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz012345";

    public static void main(String[] args) {
        int reps = args.length > 0 ? Integer.parseInt(args[0]) : 20000;
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 4; i++) sb.append(LDC);
        String fromSb = sb.toString();
        String fromConcat = LDC + LDC + LDC + LDC;
        String fromSubstr = fromConcat.substring(0);
        String fromNew = new String(fromConcat.toCharArray());

        String[] names = {"ldc(x4 concat)", "sb.toString()", "substring(0)", "new String(char[])"};
        String[] vals = {fromConcat, fromSb, fromSubstr, fromNew};
        for (int w = 0; w < 3; w++) for (String v : vals) scan(v, 200);
        for (int k = 0; k < vals.length; k++) {
            long t = System.nanoTime();
            long h = scan(vals[k], reps);
            long ns = System.nanoTime() - t;
            long n = (long) vals[k].length() * reps;
            System.out.println("REPR " + names[k] + " len=" + vals[k].length()
                + " ns_per_charAt=" + (ns / n) + " H=" + h);
        }
    }

    static long scan(String s, int reps) {
        long h = 1469598103934665603L;
        int n = s.length();
        for (int r = 0; r < reps; r++)
            for (int i = 0; i < n; i++) { h ^= (s.charAt(i) & 0xFF); h *= 1099511628211L; }
        return h;
    }
}
