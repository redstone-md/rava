// The exceptional array paths the hot-loop fast path must NOT change.
//
// `opcodes::hot` now serves an in-bounds array load without leaving the dispatch loop, and returns
// to the cold path — stack and pc untouched — for every other case. That is only sound if the
// other cases still behave identically, and "identically" here means the exception CLASS and its
// MESSAGE, because guest code catches on both.
public class ArrEx {
    public static void main(String[] a) {
        int[] x = new int[3];
        try {
            int v = x[5];
            System.out.println("NO-THROW " + v);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("AIOOBE " + e.getMessage());
        }
        try {
            int v = x[-1];
            System.out.println("NO-THROW " + v);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("NEG " + e.getMessage());
        }
        int[] n = null;
        try {
            int v = n[0];
            System.out.println("NO-THROW " + v);
        } catch (NullPointerException e) {
            System.out.println("NPE");
        }
        Object[] o = new Object[2];
        o[1] = "s";
        System.out.println("REF " + o[1] + " " + o[0]);
        System.out.println("OK " + x[2]);
    }
}
