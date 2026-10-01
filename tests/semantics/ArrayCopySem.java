/** System.arraycopy's exceptions and their messages (programs log them), in first-tier and
 *  optimized code: null arrays, non-arrays, type mismatches, negative and out-of-range
 *  positions and lengths, element store failures with the prefix copied, and overlap. */
public class ArrayCopySem {
    static String copy(Object s, int sp, Object d, int dp, int n) {
        try {
            System.arraycopy(s, sp, d, dp, n);
            return "ok";
        } catch (RuntimeException e) {
            return e.getClass().getSimpleName() + ": " + e.getMessage();
        }
    }

    public static void main(String[] args) {
        int[] i8 = new int[8];
        long[] l4 = new long[4];
        Object[] o3 = {1, "a", null};
        String[] s3 = new String[3];
        Object[][] cases = {
            {null, 0, i8, 0, 1}, {i8, 0, null, 0, 1}, {"x", 0, i8, 0, 1}, {i8, 0, "x", 0, 1},
            {i8, 0, l4, 0, 1}, {i8, 0, o3, 0, 1}, {o3, 0, i8, 0, 1},
            {i8, -1, i8, 0, 1}, {i8, 0, l4.clone(), -2, 1}, {i8, 0, i8, 0, -3},
            {i8, 5, i8, 0, 4}, {i8, 0, new int[2], 0, 3}, {o3, 2, s3, 0, 2}, {o3, 1, new Object[1], 0, 2},
            {o3, 0, s3, 0, 3}, {i8, 0, i8, 2, 6}, {o3, 1, s3, 0, 2},
        };
        StringBuilder sb = new StringBuilder();
        for (int round = 0; round < 3000; round++) {
            for (Object[] c : cases) {
                String r = copy(c[0], (Integer) c[1], c[2], (Integer) c[3], (Integer) c[4]);
                if (round == 2999) sb.append(r).append('\n');
            }
        }
        System.out.print(sb);
        System.out.println(java.util.Arrays.toString(s3));
    }
}
