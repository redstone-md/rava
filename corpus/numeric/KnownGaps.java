/**
 * Numeric divergences that are NOT fixable without changing how rava represents a
 * value, kept out of run.sh's green set so the gate stays meaningful. Run it by
 * name to see the current state:  sh corpus/numeric/run.sh KnownGaps
 *
 * 1. A BOXED CHARACTER cannot be told from a boxed int. rava's `Value` has Int,
 *    Long, Float and Double but no Char, and `Character.valueOf(C)` is an identity
 *    on the Value::Int the `i2c` produced. By the time `String.valueOf(Object)`
 *    sees it there is nothing left that says "char", so it renders 98 where the
 *    JVM renders 'b'. Every UNBOXED char path is correct (println(char), string
 *    concatenation, StringBuilder.append, char[], String.charAt) because those
 *    carry the type in the descriptor. Fixing this means boxing Character into a
 *    real object the way `Boolean` already is, which changes identity, map keys
 *    and unboxing everywhere — a representation change, not a patch.
 *
 * 2. CROSS-TYPE BOXED equals. Integer.equals(Object) must be false for anything
 *    that is not an Integer, but a boxed Integer, Short, Byte and Character are
 *    all the same Value::Int, so the arm that serves `equals` cannot separate
 *    them. (Integer vs Long and Integer vs Double already answer false — those
 *    tags do differ.) Same representation change as above.
 */
public class KnownGaps {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        p("boxed_char", (char) ('a' + 1));                 // JVM b   / rava 98
        p("boxed_char_ternary", false ? 'a' : 98);          // JVM b   / rava 98
        p("eq_int_short", Integer.valueOf(1).equals(Short.valueOf((short) 1)));   // false / true
        p("eq_int_byte", Integer.valueOf(1).equals(Byte.valueOf((byte) 1)));      // false / true
        p("eq_int_char", Integer.valueOf(97).equals(Character.valueOf('a')));     // false / true
        p("eq_int_long", Integer.valueOf(1).equals(Long.valueOf(1L)));            // false / true
        System.out.println("DONE");
    }
}
