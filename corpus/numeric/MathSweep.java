/** Randomised differential over the integer Math helpers and the format engine. */
public class MathSweep {
    static long s = 0x243F6A8885A308D3L;
    static long next() { s ^= s << 13; s ^= s >>> 7; s ^= s << 17; return s; }

    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 8000; i++) {
            long x = next(), y = next();
            if ((i & 3) == 0) y = (y % 17) - 8;          // small divisors, incl. 0 and -1
            if ((i & 7) == 1) x = (x % 1000) - 500;
            int xi = (int) x, yi = (int) y;
            sb.append(i).append(':');
            try { sb.append(Math.floorDiv(x, y)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.floorMod(x, y)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.floorDiv(xi, yi)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.floorMod(xi, yi)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.ceilDiv(x, y)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.ceilMod(x, y)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.addExact(x, y)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.multiplyExact(x, y)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.subtractExact(xi, yi)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.multiplyExact(xi, yi)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',');
            try { sb.append(Math.toIntExact(x)); } catch (ArithmeticException e) { sb.append('E'); }
            sb.append(',').append(Math.multiplyHigh(x, y));
            sb.append(',').append(Integer.compareUnsigned(xi, yi));
            sb.append(',').append(Long.compareUnsigned(x, y));
            sb.append(',').append(Integer.toUnsignedString(xi, 36));
            sb.append(',').append(Long.toUnsignedString(x, 16));
            sb.append(',').append(Integer.divideUnsigned(xi, yi == 0 ? 1 : yi));
            sb.append(',').append(Math.abs(xi)).append(',').append(Math.abs(x));
            sb.append('\n');
        }
        // formatting sweep over the same stream of doubles
        for (int i = 0; i < 4000; i++) {
            double d = Double.longBitsToDouble(next());
            if (!Double.isFinite(d)) d = (next() % 1000000) / 1000.0;
            sb.append(String.format(java.util.Locale.ROOT, "%f|%.2f|%.0f|%e|%.3e|%g|%012.3f|%,.2f|%+.1f",
                    d, d, d, d, d, d, d, d, d)).append('\n');
        }
        for (int i = 0; i < 4000; i++) {
            long v = next();
            sb.append(String.format(java.util.Locale.ROOT, "%d|%08d|%x|%X|%o|%,d|%+d|%-12d|",
                    v, (int) v, (int) v, v, (int) v, v, (int) v, (int) v)).append('\n');
        }
        System.out.print(sb);
        System.out.println("DONE");
    }
}
