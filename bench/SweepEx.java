// A broad differential sweep over the natively-served JDK surface that the
// batch's five probes do NOT cover: text formatting, radix conversion, the
// StringBuilder edit family, Arrays, Optional and the char classifiers.
//
// Deterministic text only — run on both engines and diff line by line.
import java.util.*;

public class SweepEx {
    static void p(String tag, java.util.function.Supplier<Object> s) {
        try { System.out.println(tag + " " + s.get()); }
        catch (Throwable t) { System.out.println(tag + " !" + t.getClass().getName() + ":" + t.getMessage()); }
    }

    public static void main(String[] a) {
        // --- radix and parsing -------------------------------------------------
        p("hex", () -> Integer.toHexString(-1) + " " + Long.toHexString(-1L) + " " + Integer.toHexString(0));
        p("oct-bin", () -> Integer.toOctalString(-8) + " " + Integer.toBinaryString(5) + " " + Integer.toBinaryString(-5));
        p("radix", () -> Integer.toString(255, 16) + " " + Integer.toString(-255, 36) + " " + Long.toString(Long.MIN_VALUE, 2).length());
        p("parse", () -> Integer.parseInt("-2147483648") + " " + Long.parseLong("9223372036854775807")
            + " " + Integer.parseInt("ff", 16) + " " + Integer.parseInt("+7"));
        p("parse-bad", () -> Integer.parseInt("2147483648"));
        p("parse-empty", () -> Integer.parseInt(""));
        p("parse-space", () -> Integer.parseInt(" 1"));
        p("parse-null", () -> Integer.parseInt(null));
        p("valueOf-cache", () -> (Integer.valueOf(127) == Integer.valueOf(127)) + " " + (Integer.valueOf(128) == Integer.valueOf(128)));
        p("unsigned", () -> Integer.toUnsignedString(-1) + " " + Integer.parseUnsignedInt("4294967295")
            + " " + Integer.divideUnsigned(-1, 2) + " " + Integer.compareUnsigned(-1, 1));
        p("bits", () -> Integer.bitCount(-1) + " " + Integer.numberOfLeadingZeros(1)
            + " " + Integer.numberOfTrailingZeros(8) + " " + Integer.reverse(1)
            + " " + Integer.highestOneBit(100) + " " + Long.bitCount(-1L));
        p("double-parse", () -> Double.parseDouble("1e309") + " " + Double.parseDouble("-0.0")
            + " " + Double.parseDouble("  3.5  ") + " " + Float.parseFloat("0.1"));

        // --- formatting --------------------------------------------------------
        p("fmt-int", () -> String.format("%d|%5d|%-5d|%05d|%,d", 42, 42, 42, 42, 1234567));
        p("fmt-float", () -> String.format("%f|%.2f|%10.3f|%e|%g", 1.5, 1.005, 3.14159, 12345.6789, 0.000123));
        p("fmt-str", () -> String.format("%s|%10s|%-10s|%S|%.3s", "ab", "ab", "ab", "ab", "abcdef"));
        p("fmt-misc", () -> String.format("%x|%X|%o|%c|%b|%%|%n", 255, 255, 8, 'z', true).trim());
        p("fmt-plus", () -> String.format("%+d|%(d|% d", 5, -5, 5));
        p("fmt-bad", () -> String.format("%d", "notanumber"));
        p("fmt-missing", () -> String.format("%d %d", 1));

        // --- StringBuilder edits ----------------------------------------------
        p("sb-insert", () -> new StringBuilder("hello").insert(2, "XY").toString());
        p("sb-insert-end", () -> new StringBuilder("abc").insert(3, "!").toString());
        p("sb-insert-oob", () -> new StringBuilder("abc").insert(9, "!").toString());
        p("sb-delete", () -> new StringBuilder("abcdef").delete(1, 3).toString());
        p("sb-delete-past", () -> new StringBuilder("abc").delete(1, 99).toString());
        p("sb-delete-bad", () -> new StringBuilder("abc").delete(2, 1).toString());
        p("sb-replace", () -> new StringBuilder("abcdef").replace(1, 3, "ZZZ").toString());
        p("sb-appendcp", () -> new StringBuilder().appendCodePoint(65).append(1).append(2.5).append('c').append(true).toString());
        p("sb-indexof", () -> new StringBuilder("abcabc").indexOf("bc") + " " + new StringBuilder("abcabc").lastIndexOf("bc")
            + " " + new StringBuilder("abc").indexOf("zz"));
        p("sb-chain", () -> new StringBuilder("abc").reverse().append("|").append(new StringBuilder("xy").reverse()).toString());

        // --- String surface ----------------------------------------------------
        p("str-pad", () -> "|" + " x ".strip() + "|" + " x ".trim() + "|" + "x".repeat(3) + "|" + "".isBlank() + "|" + "  ".isBlank());
        p("str-join", () -> String.join("-", "a", "b") + " " + String.join(",", List.of("x", "y", "z")));
        p("str-case", () -> "Straße".toUpperCase() + " " + "İ".toLowerCase().length() + " " + "ABC".equalsIgnoreCase("abc"));
        p("str-cmp", () -> "a".compareTo("b") + " " + "b".compareTo("a") + " " + "abc".compareTo("ab")
            + " " + "A".compareToIgnoreCase("a"));
        p("str-region", () -> "hello".regionMatches(1, "bell", 1, 3) + " " + "hello".regionMatches(true, 0, "HEL", 0, 3));
        p("str-chars", () -> "abc".chars().sum() + " " + "abc".codePointAt(0) + " " + "abc".indexOf('b'));
        p("str-split", () -> Arrays.toString("a,b,,c,,".split(","))
            + Arrays.toString("a,b".split(",", -1))
            + Arrays.toString("".split(","))
            + Arrays.toString("abc".split("")));
        p("str-replace", () -> "aaa".replace("a", "b") + " " + "a.c".replace(".", "-")
            + " " + "a.c".replaceAll("\\.", "-") + " " + "abc".replaceFirst("[bc]", "_"));
        p("str-matches", () -> "abc123".matches("[a-z]+\\d+") + " " + "abc".contains("bc") + " " + "abc".endsWith("bc"));
        p("str-value", () -> String.valueOf(1.0) + " " + String.valueOf('c') + " " + String.valueOf(true)
            + " " + String.valueOf(new char[]{'x','y'}) + " " + String.valueOf((Object) null));

        // --- Arrays -------------------------------------------------------------
        p("arr-tostring", () -> Arrays.toString(new int[]{3,1,2}) + Arrays.toString(new String[]{"a",null})
            + Arrays.deepToString(new Object[]{new int[]{1}, "x"}));
        p("arr-sort", () -> { int[] x = {3,1,2}; Arrays.sort(x); return Arrays.toString(x); });
        p("arr-binsearch", () -> Arrays.binarySearch(new int[]{1,3,5}, 3) + " " + Arrays.binarySearch(new int[]{1,3,5}, 4));
        p("arr-fill-copy", () -> { int[] x = new int[3]; Arrays.fill(x, 7);
            return Arrays.toString(Arrays.copyOf(x, 5)) + Arrays.toString(Arrays.copyOfRange(x, 1, 3)); });
        p("arr-equals", () -> Arrays.equals(new int[]{1,2}, new int[]{1,2}) + " "
            + Arrays.deepEquals(new Object[]{new int[]{1}}, new Object[]{new int[]{1}})
            + " " + Arrays.hashCode(new int[]{1,2}));
        p("arr-aslist", () -> { List<String> l = Arrays.asList("a","b"); l.set(0,"z"); return l.toString(); });
        p("arr-stream", () -> Arrays.stream(new int[]{1,2,3}).sum() + " " + Arrays.stream(new String[]{"a","b"}).count());

        // --- Optional -----------------------------------------------------------
        p("opt", () -> Optional.of("v").map(String::toUpperCase).orElse("-") + " "
            + Optional.empty().orElse("-") + " " + Optional.ofNullable(null).isPresent()
            + " " + Optional.of("v").filter(x -> false).isPresent());
        p("opt-throw", () -> Optional.empty().get());
        p("opt-tostring", () -> Optional.of(1).toString() + " " + Optional.empty().toString());

        // --- characters ---------------------------------------------------------
        p("char", () -> Character.isDigit('5') + " " + Character.isLetter('x')
            + " " + Character.isWhitespace('\u001c') + " " + Character.isWhitespace('\u00a0')
            + " " + Character.isWhitespace(' ') + " " + Character.toUpperCase('\u00df')
            + " " + Character.getNumericValue('7') + " " + Character.isUpperCase('A'));

        // --- math edges ----------------------------------------------------------
        p("math", () -> Math.floorDiv(-7, 2) + " " + Math.floorMod(-7, 2) + " " + Math.floorDiv(Long.MIN_VALUE, -1L)
            + " " + Math.abs(Integer.MIN_VALUE) + " " + Math.round(-0.5) + " " + Math.round(2.5)
            + " " + (int) Math.signum(-3.0) + " " + Math.max(-0.0, 0.0));
        p("math-throw", () -> 1 / (Integer.parseInt("1") - 1));
        p("math-exact", () -> Math.addExact(1, 2) + " " + Math.toIntExact(5L));
        p("math-overflow", () -> Math.addExact(Integer.MAX_VALUE, 1));
    }
}
