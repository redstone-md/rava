// Pure-JDK shape of RandomSupport.seedFromHashOf: byte[] -> 16 baloads ->
// 8-byte-arg static combine (guava Longs.fromBytes clone), cold vs warm.
// Diverges => generic JIT bug (baload / 8-arg inline / byte marshaling);
// clean => the MC repro's bug sits in the guava asBytes()/hashString natives.
public class PureBytes {
    static long fromBytes(byte b1, byte b2, byte b3, byte b4, byte b5, byte b6, byte b7, byte b8) {
        return (b1 & 0xFFL) << 56 | (b2 & 0xFFL) << 48 | (b3 & 0xFFL) << 40 | (b4 & 0xFFL) << 32
            | (b5 & 0xFFL) << 24 | (b6 & 0xFFL) << 16 | (b7 & 0xFFL) << 8 | (b8 & 0xFFL);
    }

    static long pair(byte[] a) {
        long lo = fromBytes(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
        long hi = fromBytes(a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15]);
        return lo * 31 + hi;
    }

    public static void main(String[] s) {
        byte[] a = new byte[16];
        for (int i = 0; i < 16; i++) a[i] = (byte) (0x88 + i * 7); // high bit mix
        long cold = pair(a);
        long warm = 0;
        for (int i = 0; i < 60000; i++) warm = pair(a);
        System.out.println("COLD=" + Long.toHexString(cold) + " WARM=" + Long.toHexString(warm)
            + (cold == warm ? " OK" : " DIVERGED"));
    }
}
