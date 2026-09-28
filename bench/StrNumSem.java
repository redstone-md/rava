// Differential probe: string, number and formatting semantics. Pure JDK, seconds to run.
// These are the paths identifiers, NBT text and JSON decode live on.
//
// TWO LINES ARE EXPECTED TO DIVERGE from `java -Xint`, and both are architectural rather
// than defects — a third divergence means something broke:
//
//   valueOf.cache  the JVM prints "true false": Integer.valueOf(128) == Integer.valueOf(128)
//                  is FALSE outside the -128..127 cache, because those are distinct boxes.
//                  rava's value model does not box, so `==` compares values and prints
//                  "true true". Matching would mean giving boxed integers identity.
//
//   fmt.f          the JVM honours the DEFAULT LOCALE, so on a machine set to a
//                  comma-decimal locale it prints 3,141590. rava always formats as
//                  Locale.ROOT. Matching would mean asking the OS for its locale, which
//                  is a decision about what the engine is, not a bug fix.
//
// Machine-readable form of the two notes above, for bench\sem_all.ps1. This probe predates
// that runner and was therefore never in it — which is exactly how a suite drifts from
// "every probe" to "the ones somebody remembered".
// DIVERGES: valueOf.cache
// DIVERGES: fmt.f
import java.util.*;
public class StrNumSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    public static void main(String[] a) {
        // --- Double/Float toString: the shortest form that round-trips
        for (double d : new double[]{ 0.1, 1.0, 100.0, 1e7, 1e-3, 1e-4, 1.0e20, 1.0e21, 3.0/7, -0.0, 1.5e-10 })
            p("dbl", d);
        p("dbl.nan", Double.NaN + " " + Double.POSITIVE_INFINITY + " " + Double.NEGATIVE_INFINITY);
        for (float f : new float[]{ 0.1f, 1.0f, 1e7f, 1e-3f, 3.0f/7 }) p("flt", f);
        p("dbl.minmax", Double.MIN_VALUE + " " + Double.MAX_VALUE);
        p("long.minmax", Long.MIN_VALUE + " " + Long.MAX_VALUE);
        p("int.minmax", Integer.MIN_VALUE + " " + Integer.MAX_VALUE);

        // --- parsing, including the failures
        p("parseInt", Integer.parseInt("-42") + " " + Integer.parseInt("+7"));
        try { Integer.parseInt("12a"); p("parseInt.bad", "NO THROW"); }
        catch (Exception e) { p("parseInt.bad", e.getClass().getName() + ": " + e.getMessage()); }
        try { Integer.parseInt(""); p("parseInt.empty", "NO THROW"); }
        catch (Exception e) { p("parseInt.empty", e.getClass().getName() + ": " + e.getMessage()); }
        try { Integer.parseInt("2147483648"); p("parseInt.overflow", "NO THROW"); }
        catch (Exception e) { p("parseInt.overflow", e.getClass().getName() + ": " + e.getMessage()); }
        p("parseDouble", Double.parseDouble("1e3") + " " + Double.parseDouble("-0.5"));
        p("radix", Integer.parseInt("ff", 16) + " " + Integer.toHexString(255) + " " + Integer.toBinaryString(5)
            + " " + Integer.toOctalString(8) + " " + Long.toHexString(-1L));
        p("valueOf.cache", (Integer.valueOf(127) == Integer.valueOf(127)) + " " + (Integer.valueOf(128) == Integer.valueOf(128)));

        // --- integer maths that wraps or throws
        p("div", (-7 / 2) + " " + (-7 % 2) + " " + Math.floorDiv(-7, 2) + " " + Math.floorMod(-7, 2));
        try { int z = 1 / 0; p("div0", z); } catch (Exception e) { p("div0", e.getClass().getName() + ": " + e.getMessage()); }
        p("shift", (-8 >> 1) + " " + (-8 >>> 28) + " " + (1 << 33) + " " + (1L << 33));
        p("overflow", (Integer.MAX_VALUE + 1) + " " + (Long.MAX_VALUE + 1));
        p("cast", ((int) 3.99) + " " + ((int) -3.99) + " " + ((byte) 200) + " " + ((char) 65) + " " + ((int) Double.NaN));
        p("compare", Integer.compare(1, 2) + " " + Double.compare(0.0, -0.0) + " " + Double.compare(Double.NaN, Double.NaN));

        // --- String basics and the corners
        String s = "Hello, World";
        p("substr", s.substring(7) + "|" + s.substring(0, 5) + "|" + s.substring(5, 5));
        p("indexOf", s.indexOf('o') + " " + s.lastIndexOf('o') + " " + s.indexOf("lo") + " " + s.indexOf("zz"));
        p("split", Arrays.toString("a,b,,c,".split(",")) + " " + Arrays.toString("a,b,,c,".split(",", -1)));
        p("split.empty", Arrays.toString("".split(",")) + " " + "abc".split("").length);
        p("replace", s.replace('l', 'L') + " " + s.replace("World", "There"));
        p("trim/strip", "  x  ".trim() + "|" + "  x  ".strip() + "|" + " ".isBlank() + " " + "".isEmpty());
        p("case", "Straße".toUpperCase() + " " + "İ".toLowerCase().length());
        p("compareTo", "apple".compareTo("banana") + " " + "a".compareTo("A") + " " + "abc".compareToIgnoreCase("ABC"));
        p("join", String.join("-", "a", "b") + " " + String.join(",", List.of("x", "y")));
        p("repeat/chars", "ab".repeat(3) + " " + "abc".chars().sum());
        p("hash", "hello".hashCode() + " " + "".hashCode() + " " + "Aa".hashCode() + " " + "BB".hashCode());
        try { s.substring(20); p("substr.oob", "NO THROW"); }
        catch (Exception e) { p("substr.oob", e.getClass().getName() + ": " + e.getMessage()); }
        try { s.charAt(-1); p("charAt.oob", "NO THROW"); }
        catch (Exception e) { p("charAt.oob", e.getClass().getName() + ": " + e.getMessage()); }

        // --- String.format / printf
        p("fmt.d", String.format("%d|%5d|%-5d|%05d|%+d", 42, 42, 42, 42, 42));
        p("fmt.f", String.format("%f|%.2f|%10.3f|%e|%g", 3.14159, 3.14159, 3.14159, 31415.9, 0.000123));
        p("fmt.s", String.format("%s|%10s|%-10s|%S", "hi", "hi", "hi", "hi"));
        p("fmt.x", String.format("%x|%X|%o|%c|%b|%%", 255, 255, 8, 'z', true));
        p("fmt.null", String.format("%s|%d", null, 0));

        // --- StringBuilder mutation
        StringBuilder b = new StringBuilder("abcdef");
        b.insert(3, "XY").delete(0, 1).replace(1, 3, "ZZ").reverse();
        p("sb", b + " len=" + b.length());
        b.setLength(3);
        p("sb.setLength", b);
        b.setCharAt(0, '!');
        p("sb.setCharAt", b + " " + b.indexOf("!"));
        p("sb.deleteCharAt", new StringBuilder("abc").deleteCharAt(1));
        p("sb.appendNull", new StringBuilder().append((Object) null).append((String) null).toString());

        // --- Character classification
        p("char", Character.isDigit('5') + " " + Character.isLetter('é') + " " + Character.isWhitespace('\t')
            + " " + Character.toUpperCase('ß') + " " + (int) Character.toLowerCase('A'));

        // --- Objects / Arrays
        p("objects", Objects.toString(null, "d") + " " + Objects.hashCode(null) + " " + Objects.equals(null, null));
        int[] xs = { 5, 3, 1, 4 };
        int[] ys = xs.clone();
        Arrays.sort(ys);
        p("arrays", Arrays.toString(ys) + " " + Arrays.binarySearch(ys, 4) + " " + Arrays.binarySearch(ys, 2));
        p("arrays2", Arrays.toString(Arrays.copyOf(xs, 6)) + " " + Arrays.toString(Arrays.copyOfRange(xs, 1, 3)));
        p("arrays.equals", Arrays.equals(xs, xs.clone()) + " " + Arrays.hashCode(xs) + " " + Arrays.deepToString(new int[][]{{1,2},{3}}));
    }
}
