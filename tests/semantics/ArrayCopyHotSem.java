// System.arraycopy from optimized code: the intrinsic copies when every check passes, and every
// failure (bounds, null, kinds, element store checks) still throws as the interpreter does.
public class ArrayCopyHotSem {
    static int copy(Object s, int sp, Object d, int dp, int n) {
        try { System.arraycopy(s, sp, d, dp, n); return 0; }
        catch (ArrayIndexOutOfBoundsException e) { return 1; }
        catch (ArrayStoreException e) { return 2 + e.getMessage().length() % 7; }
        catch (NullPointerException e) { return 9; }
    }
    public static void main(String[] a) {
        int[] x = new int[16], y = new int[16]; Object[] o = new Object[8]; String[] st = new String[8];
        for (int i = 0; i < 16; i++) x[i] = i;
        long acc = 0;
        for (int r = 0; r < 200000; r++) acc += copy(x, r & 3, y, 2, 8) + y[5];
        System.out.println(acc + " " + y[9]);
        o[0] = "a"; o[1] = 7; o[2] = "c";
        System.out.println(copy(x, 10, y, 0, 8) + " " + copy(x, 0, y, -1, 1) + " " + copy(null, 0, y, 0, 1) + " " + copy(x, 0, o, 0, 1));
        System.out.println(copy(o, 0, st, 0, 3) + " " + st[0] + " " + st[1]);
        System.out.println(copy(st, 0, o, 4, 2) + " " + o[4] + " " + copy(y, 0, y, 1, 10) + " " + y[1] + y[2] + y[3]);
    }
}
