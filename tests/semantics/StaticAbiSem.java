// Register ABI coverage: mixed primitive widths, rejected non-leaf entries,
// and reference arguments/results. static_abi_sem.ps1 verifies route diagnostics.
public class StaticAbiSem {
    static long visits;

    static final class Cell {
        final Object keep;
        final double value;
        Cell(Object keep, double value) {
            this.keep = keep;
            this.value = value;
        }
    }

    static double leaf(int x, long y, float z, double w) {
        double sum = w;
        for (int i = 0; i < 7; i++) {
            sum += (x ^ i) + (y & 255) + z;
        }
        return sum;
    }

    static double nonleaf(int x, long y, float z, double w) {
        visits++;
        Cell allocation = new Cell(null, 0.5);
        return leaf(x, y, z, w) + allocation.value;
    }

    static Object reference(Object keep, int n) {
        Object[] roots = new Object[n];
        for (int i = 0; i < n; i++) roots[i] = new Cell(keep, i);
        return ((Cell) roots[n - 1]).keep;
    }

    static double batch(Object token, int start) {
        double sum = 0;
        for (int i = start; i < start + 20; i++) {
            sum += leaf(i & 31, 0x100000007L + i, 1.25f, -2.5);
            // Both references remain on the caller operand stack across fallback GC.
            sum += checked(token, reference(token, 8),
                    nonleaf(i & 15, -1234567890123L + i, -0.5f, 0.25));
            if (reference(token, 8) != token) throw new AssertionError();
        }
        return sum;
    }

    static double checked(Object expected, Object actual, double value) {
        if (expected != actual) throw new AssertionError();
        return value;
    }

    public static void main(String[] args) {
        double sum = 0;
        Object token = new Object();
        Object last = null;
        for (int i = 0; i < 20000; i += 20) {
            sum += batch(token, i);
            last = reference(token, 8);
        }
        System.out.println("sum=" + Double.doubleToLongBits(sum));
        System.out.println("visits=" + visits);
        System.out.println("identity=" + (last == token));
    }
}
