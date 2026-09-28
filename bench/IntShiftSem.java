import it.unimi.dsi.fastutil.HashCommon;

public class IntShiftSem {
    // A loop keeps runtime trivial-method shortcuts from replacing the AOT entry.
    static int left(int value, int count) {
        int result = 0; for (int i = 0; i < 1; i++) result = value << count; return result;
    }
    static int signed(int value, int count) {
        int result = 0; for (int i = 0; i < 1; i++) result = value >> count; return result;
    }
    static int unsigned(int value, int count) {
        int result = 0; for (int i = 0; i < 1; i++) result = value >>> count; return result;
    }
    public static void main(String[] args) {
        int[] values = {Integer.MIN_VALUE, Integer.MIN_VALUE + 1, -123456789, -1, 0, 1, 123456789, Integer.MAX_VALUE};
        int[] counts = {Integer.MIN_VALUE, -65, -33, -32, -31, -1, 0, 1, 15, 16, 30, 31, 32, 33, 63, 64, Integer.MAX_VALUE};
        long hash = 1;
        for (int value : values) {
            for (int count : counts) {
                hash = hash * 31 + left(value, count);
                hash = hash * 31 + signed(value, count);
                hash = hash * 31 + unsigned(value, count);
            }
            System.out.println("mix(" + value + ")=" + HashCommon.mix(value));
        }
        System.out.println("hash=" + hash);
    }
}
