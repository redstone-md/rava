// Masked array indices (`a[x & 255]`) on arrays shorter than the mask: the optimizing tier
// checks the mask against the length once for all such accesses, which fails for these arrays
// even when the index is in bounds. The results and the exceptions must stay exact.
public class MaskIndexSem {
    static int read(int[] a, int x) {
        return a[x & 255] + a[(x + 1) & 255] * 3 + a[(x + 2) & 255] * 7;
    }

    static int write(byte[] b, int x) {
        b[x & 15] = (byte) x;
        b[(x + 3) & 15] += 1;
        return b[x & 15] + b[(x + 3) & 15];
    }

    public static void main(String[] args) {
        int[] full = new int[256];
        int[] tiny = new int[12];
        for (int i = 0; i < full.length; i++) full[i] = i * 13;
        for (int i = 0; i < tiny.length; i++) tiny[i] = i + 100;
        long sum = 0;
        int thrown = 0;
        for (int round = 0; round < 40_000; round++) {
            sum += read(full, round);
            // In bounds for the short array while x & 255 < 10.
            sum += read(tiny, round % 10);
            try {
                sum += read(tiny, round);
            } catch (ArrayIndexOutOfBoundsException e) {
                thrown++;
            }
        }
        byte[] b16 = new byte[16];
        byte[] b8 = new byte[8];
        int bt = 0;
        for (int round = 0; round < 40_000; round++) {
            sum += write(b16, round);
            try {
                sum += write(b8, round);
            } catch (ArrayIndexOutOfBoundsException e) {
                bt++;
            }
        }
        System.out.println(sum + " " + thrown + " " + bt);
    }
}
