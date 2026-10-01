public class JitBoundsSem {
    static int after;
    static int read(int[] a, int i) { int v = a[i]; after++; return v; }
    static void write(long[] a, int i) { a[i] = 99; after++; }
    static Object reference(Object[] a, int i) { Object v = a[i]; after++; return v; }

    public static void main(String[] args) {
        int[] ints = new int[3];
        long[] longs = new long[7];
        Object[] refs = new Object[2];
        int correct = 0;
        for (int n = 0; n < 4096; n++) {
            read(ints, 0);
            write(longs, 0);
            reference(refs, 0);
            try { read(ints, 3); }
            catch (ArrayIndexOutOfBoundsException e) {
                if ("Index 3 out of bounds for length 3".equals(e.getMessage())) correct++;
            }
            try { write(longs, -1); }
            catch (ArrayIndexOutOfBoundsException e) {
                if ("Index -1 out of bounds for length 7".equals(e.getMessage())) correct++;
            }
            try { reference(refs, 2); }
            catch (ArrayIndexOutOfBoundsException e) {
                if ("Index 2 out of bounds for length 2".equals(e.getMessage())) correct++;
            }
        }
        System.out.println("messages=" + correct);
        System.out.println("after=" + after);
        System.out.println("value=" + longs[0]);
    }
}
