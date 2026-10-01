// Differential probe: the interpreter's own arithmetic — overflow, shifts, conversions,
// float special values, compound-assignment narrowing. Core semantics: a divergence here
// is not a missing API, it is every computation being subtly wrong.
//
// EVERY LINE MATCHES `java -Xint`; there are no declared divergences.
//
// ⭐**`compound.char` WAS DECLARED HERE AND IS NOW CLOSED.** It read: "the JVM prints `b`, this
// engine prints `98`... printing it goes through `Character.valueOf`, and this engine's value
// model has no char, so the box is an int and renders as one. Closing it means giving boxed
// chars a type."
//
// That is `crate::boxid`: `Character.valueOf` mints a real `java/lang/Character` object whose
// `toString` renders the character, so `char cc = 'a'; cc += 1;` prints `b`. The arithmetic was
// never wrong — only the render of the box was — and the line is COMPARED now, as the
// regression test for that work. `RAVA_NO_BOXID=1` makes it diverge again.
public class NumSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    public static void main(String[] a) {
        // --- integer overflow wraps, in every operator
        int max = Integer.MAX_VALUE, min = Integer.MIN_VALUE;
        p("int.wrap", (max + 1) + " " + (min - 1) + " " + (max * 2) + " " + (-min) + " " + Math.abs(min));
        long lmax = Long.MAX_VALUE, lmin = Long.MIN_VALUE;
        p("long.wrap", (lmax + 1) + " " + (lmin - 1) + " " + (lmax * 2) + " " + (-lmin));
        p("div.edge", (min / -1) + " " + (min % -1) + " " + (lmin / -1L));

        // --- shifts mask their count
        p("shift.mask", (1 << 32) + " " + (1 << 33) + " " + (1L << 64) + " " + (1L << 65));
        p("shift.neg", (-16 >> 2) + " " + (-16 >>> 2) + " " + (-16L >>> 2));
        p("shift.by", (1 << -1) + " " + (256 >> 40));

        // --- narrowing conversions truncate
        p("narrow", ((byte) 130) + " " + ((short) 70000) + " " + ((char) -1 + 0) + " " + ((int) 4294967296L));
        p("f2i", ((int) 1e20) + " " + ((int) -1e20) + " " + ((long) 1e30) + " " + ((int) Float.NaN));
        p("i2f", (float) 16777217 + " " + (double) Long.MAX_VALUE + " " + (float) 1234567890123L);

        // --- compound assignment has an implicit cast
        byte bb = 10; bb += 300; p("compound.byte", bb);
        char cc = 'a'; cc += 1; p("compound.char", cc);
        int ii = 7; ii /= 2; p("compound.div", ii);
        short ss = 1; ss *= 100000; p("compound.short", ss);

        // --- char arithmetic promotes to int
        char c = 'A';
        p("char.math", (c + 1) + " " + (char) (c + 1) + " " + (c < 'B') + " " + Character.getNumericValue('7'));

        // --- float and double special values
        double nan = Double.NaN, inf = Double.POSITIVE_INFINITY;
        p("nan.cmp", (nan == nan) + " " + (nan != nan) + " " + (nan < 1) + " " + (nan > 1));
        p("nan.box", Double.valueOf(nan).equals(Double.valueOf(nan)) + " " + Double.compare(nan, nan));
        p("zero", (0.0 == -0.0) + " " + Double.valueOf(0.0).equals(Double.valueOf(-0.0))
            + " " + Double.compare(0.0, -0.0) + " " + (1 / 0.0) + " " + (1 / -0.0));
        p("inf.math", (inf - inf) + " " + (inf * 0) + " " + (0.0 / 0.0) + " " + (inf + 1));
        p("fp.arith", (0.1 + 0.2) + " " + (0.1f + 0.2f) + " " + (1e308 * 10));
        p("rem", (5.5 % 2) + " " + (-5.5 % 2) + " " + (5 % -2) + " " + (-5 % 2));

        // --- widening in mixed expressions
        p("mixed", (1 + 2L) + " " + (1 / 2) + " " + (1 / 2.0) + " " + (1L + 1.5f) + " " + ('a' + 1L));

        // --- Math, the exact and the rounding halves
        p("math.round", Math.round(2.5) + " " + Math.round(-2.5) + " " + Math.round(2.4f)
            + " " + Math.rint(2.5) + " " + Math.rint(3.5));
        p("math.floorCeil", Math.floor(-2.5) + " " + Math.ceil(-2.5) + " " + Math.floorDiv(-7, 2)
            + " " + Math.floorMod(-7, 2));
        p("math.minmax", Math.min(-0.0, 0.0) + " " + Math.max(Double.NaN, 1.0) + " " + Math.min(1, 2L));
        p("math.pow", Math.pow(2, 10) + " " + Math.pow(0, 0) + " " + Math.sqrt(-1) + " " + Math.cbrt(-8));
        p("math.exact", exact());
        p("math.bits", Integer.bitCount(255) + " " + Long.bitCount(-1L) + " " + Integer.reverse(1)
            + " " + Integer.highestOneBit(100) + " " + Integer.numberOfTrailingZeros(8));
        p("math.hypot", Math.hypot(3, 4) + " " + Math.signum(-2.5) + " " + Math.copySign(3, -0.0));

        // --- parsing and formatting round-trips
        p("roundTrip", Double.parseDouble(Double.toString(0.1)) == 0.1);
        p("bits", Double.doubleToLongBits(1.5) + " " + Float.floatToIntBits(1.5f)
            + " " + Double.longBitsToDouble(4609434218613702656L));
        p("hexFp", Double.toHexString(1.5) + " " + Double.parseDouble("0x1.8p0"));
    }
    static String exact() {
        StringBuilder b = new StringBuilder();
        try { Math.addExact(Integer.MAX_VALUE, 1); b.append("no"); }
        catch (ArithmeticException e) { b.append(e.getMessage()); }
        b.append('/');
        try { Math.toIntExact(1L << 40); b.append("no"); }
        catch (ArithmeticException e) { b.append(e.getMessage()); }
        return b.toString();
    }
}
