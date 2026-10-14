// Old objects and arrays retain young references on sparse and dense dirty cards, including
// array-card boundaries and partially occupied size-class pages. Writers own disjoint slots.
// rava-env: RAVA_GC_MB=1 RAVA_GC_THREADS=4 RAVA_GC_VERIFY=1
public class DirtyCardsSem {
    static final class Value {
        final long id;
        final byte[] data;
        Value(long id) { this.id = id; data = new byte[] { (byte) id, (byte) (id >> 8) }; }
    }
    static final class Holder {
        Value value;
        Object spare;
        long pad;
    }
    static final Holder[] holders = new Holder[8192];
    static final Object[][] arrays = new Object[32][];
    static final long[] expected = new long[holders.length];
    static final int[] positions = { 0, 123, 124, 125, 251, 252, 253, 1023, 2043 };
    static final long[][] arrayExpected = new long[arrays.length][positions.length + 1];
    static volatile Object sink;

    static void churn() {
        for (int i = 0; i < 16000; i++) sink = new byte[1024 + (i & 31)];
    }

    static void store(int writer, int round, int stride) {
        for (int i = writer; i < holders.length; i += 2) {
            if (i % stride != 0 && stride != 1) continue;
            long id = 1_000_000L * round + i + 1;
            holders[i].value = new Value(id);
            holders[i].spare = (i & 2) == 0 ? null : new Value(-id);
            expected[i] = id;
        }
        for (int i = writer; i < arrays.length; i += 2) {
            if (i % stride != 0 && stride != 1) continue;
            Object[] a = arrays[i];
            for (int j = 0; j <= positions.length; j++) {
                int at = j == positions.length ? a.length - 1 : positions[j];
                long id = 10_000_000L * round + i * 100 + j + 1;
                a[at] = new Value(id);
                arrayExpected[i][j] = id;
            }
        }
    }

    static long value(Object o, long id) {
        if (id == 0) {
            if (o != null) throw new AssertionError("unexpected reference");
            return 0;
        }
        Value v = (Value) o;
        if (v.id != id || v.data[0] != (byte) id || v.data[1] != (byte) (id >> 8)) {
            throw new AssertionError("lost young reference " + id);
        }
        return v.id;
    }

    static long check() {
        long sum = 0;
        for (int i = 0; i < holders.length; i++) {
            sum += value(holders[i].value, expected[i]);
            sum += value(holders[i].spare, expected[i] != 0 && (i & 2) != 0 ? -expected[i] : 0);
        }
        for (int i = 0; i < arrays.length; i++) {
            Object[] a = arrays[i];
            for (int j = 0; j <= positions.length; j++) {
                int at = j == positions.length ? a.length - 1 : positions[j];
                sum += value(a[at], arrayExpected[i][j]);
            }
        }
        return sum;
    }

    public static void main(String[] args) throws Exception {
        for (int i = 0; i < holders.length; i++) holders[i] = new Holder();
        for (int i = 0; i < arrays.length; i++) arrays[i] = new Object[i == 31 ? 16384 : 2048 + i];
        System.gc();
        long sum = 0;
        for (int round = 1; round <= 6; round++) {
            final int r = round, stride = (round & 1) == 0 ? 1 : 257;
            Thread t = new Thread(() -> store(1, r, stride));
            t.start();
            store(0, r, stride);
            t.join();
            churn();
            sum += check();
            // The next period also covers stores into pages reused after a full collection.
            if (round == 3) System.gc();
        }
        System.out.println("cards " + sum);
    }
}
