/**
 * Small methods with exception handlers inlined into hot callers: exceptions thrown and caught
 * inside the inlined body, finally blocks, and exceptions escaping to the caller's handler.
 */
public class OptHandlerInlineSem {
    static int finallies;

    static int parse(String s) {
        try {
            return Integer.parseInt(s);
        } catch (NumberFormatException e) {
            return -1;
        }
    }

    static int divide(int a, int b) {
        try {
            return a / b;
        } catch (ArithmeticException e) {
            return Integer.MIN_VALUE;
        } finally {
            finallies++;
        }
    }

    static int element(int[] a, int i) {
        try {
            return a[i];
        } finally {
            finallies++;
        }
    }

    public static void main(String[] args) {
        String[] inputs = {"12", "x", "-7", "", "99999999999", "42"};
        int[] arr = {1, 2, 3};
        long sum = 0;
        int escaped = 0;
        for (int i = 0; i < 300000; i++) {
            sum += parse(inputs[i % inputs.length]);
            sum += divide(i, i % 5);
            try {
                sum += element(arr, i % 4);
            } catch (ArrayIndexOutOfBoundsException e) {
                escaped++;
            }
        }
        System.out.println("sum " + sum + " escaped " + escaped + " finallies " + finallies);
    }
}
