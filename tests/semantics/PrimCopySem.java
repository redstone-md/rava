// System.arraycopy into a primitive array held in a field declared with its type (so its class
// is known exactly): shifts within the same array in both directions (overlapping ranges),
// copies between fields, and the failures: a source of another primitive type throws
// ArrayStoreException, bad ranges throw IndexOutOfBounds, a null source NPE.
public class PrimCopySem {
    static final class Buf {
        long[] a = new long[64];
        byte[] b = new byte[100];
        int size;
        Object other = new int[8];
        void removeAt(int i) { System.arraycopy(a, i + 1, a, i, size - i - 1); size--; }
        void insertAt(int i, long x) { System.arraycopy(a, i, a, i + 1, size - i); a[i] = x; size++; }
        void put(byte[] s, int at) { System.arraycopy(s, 0, b, at, s.length); }
        void shiftBytes(int from, int to, int n) { System.arraycopy(b, from, b, to, n); }
        void fromOther() { System.arraycopy(other, 0, a, 0, 4); }
    }

    public static void main(String[] args) {
        Buf f = new Buf();
        long acc = 0;
        byte[][] words = { "a".getBytes(), "hello".getBytes(), "0123456789abcdefXYZ".getBytes(), "0123456789abcdef0123456789abcdef!".getBytes() };
        for (int r = 0; r < 50000; r++) {
            f.size = 0;
            for (int i = 0; i < 20; i++) f.insertAt(i / 2, r + i);
            f.removeAt(r % 10);
            f.removeAt(0);
            for (int i = 0; i < f.size; i++) acc += f.a[i] * (i + 1);
            byte[] w = words[r & 3];
            f.put(w, r % 50);
            f.shiftBytes(r % 50, r % 37, w.length);
            f.shiftBytes(r % 37, (r % 37) + 3, w.length);
            for (int i = 0; i < f.b.length; i += 7) acc += f.b[i];
        }
        System.out.println("acc " + acc);
        try { f.fromOther(); } catch (ArrayStoreException e) { System.out.println("ase"); }
        try { f.shiftBytes(90, 0, 20); } catch (IndexOutOfBoundsException e) { System.out.println("ioobe src"); }
        try { f.shiftBytes(0, 90, 20); } catch (IndexOutOfBoundsException e) { System.out.println("ioobe dst"); }
        try { f.shiftBytes(0, 0, -1); } catch (IndexOutOfBoundsException e) { System.out.println("ioobe len"); }
        try { f.put(null, 0); } catch (NullPointerException e) { System.out.println("npe"); }
        f.b = null;
        try { f.put(words[0], 0); } catch (NullPointerException e) { System.out.println("npe dst"); }
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 2000; i++) { sb.append("x").append(i).append("é"); if (sb.length() > 500) sb.setLength(7); }
        System.out.println(sb.length() + " " + sb.toString().hashCode());
    }
}
