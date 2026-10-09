// Late folds: a constant array's length and in-range constant indexes, a fresh array's length
// (its size), arraycopy from a constant source (bounds known at compile time, including copies
// that must still fail), boolean[] stores of non-0/1 values into known and unknown arrays, and
// branches between two constants that only appear after inlining.
public class LateFoldSem {
    static final byte[] DIGITS = { '0', '1', '2', '3', '4', '5', '6', '7', '8', '9' };
    static final String PREFIX = "block.";

    static int lengths(int n) {
        byte[] fresh = new byte[n & 31];
        int[] other = new int[(n & 7) + 1];
        return fresh.length + other.length * 3 + DIGITS.length + DIGITS[3] + DIGITS[n % 10];
    }

    static int copies(int n) {
        byte[] dst = new byte[16];
        System.arraycopy(DIGITS, 2, dst, 1, 5);
        System.arraycopy(DIGITS, n & 3, dst, n & 7, 6);
        int s = dst[1] + dst[5] + dst[(n & 7) + 2];
        try {
            System.arraycopy(DIGITS, 7, dst, 0, 4);
            s += 1000;
        } catch (ArrayIndexOutOfBoundsException e) {
            s += 7;
        }
        try {
            System.arraycopy(DIGITS, 0, dst, 14, 3);
            s += 1000;
        } catch (ArrayIndexOutOfBoundsException e) {
            s += 11;
        }
        return s;
    }

    static int bools(boolean[] given, int n) {
        boolean[] fresh = new boolean[4];
        fresh[n & 3] = (n & 1) == 0;
        given[n & 3] = (n & 2) != 0;
        int s = 0;
        for (boolean b : fresh) s += b ? 1 : 0;
        for (boolean b : given) s += b ? 2 : 0;
        return s;
    }

    static boolean flag(int k) { return k > 2; }

    static int branches(int n) {
        int k = 3;
        int s = flag(k) ? n : -n;
        if (PREFIX.length() == 6) s += 5;
        if (PREFIX.isEmpty()) s -= 100;
        return s;
    }

    public static void main(String[] args) {
        boolean[] given = new boolean[4];
        long acc = 0;
        for (int r = 0; r < 60000; r++) acc += lengths(r) + copies(r) + bools(given, r) + branches(r) + (PREFIX + r).length();
        System.out.println("acc " + acc);
    }
}
