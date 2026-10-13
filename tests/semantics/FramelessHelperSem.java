// Frameless leaves that call a runtime helper: a float or double remainder (fmod), a float to
// int conversion of NaN or an out-of-range value (the saturating helper), each in a method too
// small for a frame. The helper must be entered with a 16-byte aligned stack: Windows' C runtime
// spills SSE registers with movaps, and an unaligned call faulted there (Mth.wrapDegrees).
public class FramelessHelperSem {
    static float wrap(float f) {
        float g = f % 360.0f;
        if (g >= 180.0f) g -= 360.0f;
        if (g < -180.0f) g += 360.0f;
        return g;
    }

    static double dwrap(double d) { return d % 7.25; }

    static int toInt(double d) { return (int) d; }

    static long toLong(float f) { return (long) f; }

    public static void main(String[] args) {
        double acc = 0;
        long ints = 0;
        for (int i = 0; i < 400000; i++) {
            float f = (i - 200000) * 1.37f;
            acc += wrap(f) + dwrap(i * 0.731);
            double big = (i % 3 == 0) ? Double.NaN : (i % 3 == 1 ? 1e30 * i : -1e30 * i);
            ints += toInt(big) + toLong((float) big);
        }
        System.out.println("acc " + acc + " ints " + ints);
        System.out.println(wrap(725.0f) + " " + wrap(-725.0f) + " " + dwrap(-50.0) + " "
            + toInt(Double.NaN) + " " + toInt(1e30) + " " + toLong(Float.NEGATIVE_INFINITY));
    }
}
