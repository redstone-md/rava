// THE minimal core: compiled baload on a byte[].clone() result. Bytecode-
// created arrays and digest-native arrays read fine warm; guava's
// HashCode.asBytes() is just bytes.clone() — and that's what mis-reads.
public class CloneBytes {
    static long pair(byte[] a) {
        long lo = 0, hi = 0;
        for (int i = 0; i < 8; i++) lo = lo << 8 | (a[i] & 0xFFL);
        for (int i = 8; i < 16; i++) hi = hi << 8 | (a[i] & 0xFFL);
        return lo * 31 + hi;
    }

    public static void main(String[] s) {
        byte[] src = new byte[16];
        for (int i = 0; i < 16; i++) src[i] = (byte) (0x88 + i * 7);
        long cold = pair(src.clone());
        long warm = 0, direct = 0;
        for (int i = 0; i < 60000; i++) {
            warm = pair(src.clone());
            direct = pair(src);
        }
        System.out.println("COLD=" + Long.toHexString(cold) + " CLONE_WARM=" + Long.toHexString(warm)
            + " DIRECT_WARM=" + Long.toHexString(direct));
        System.out.println(cold == warm && cold == direct ? "ALL_OK" : "DIVERGED");
    }
}
