public class CharBox {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }
    static void ps(String k, String v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        char c = (char) ('a' + 1);

        // the ordinary char paths (no boxing)
        System.out.println("println_char=" + c);
        ps("concat", "" + c);
        ps("sb", new StringBuilder().append(c).append('x').toString());
        ps("valueOf", String.valueOf(c));
        ps("Character_toString", Character.toString(c));
        ps("Character_toString_int", Character.toString(98));
        char[] arr = { 'a', 'b' };
        ps("new_String", new String(arr));
        ps("charAt", "" + "abc".charAt(1));
        System.out.print("print_char=");
        System.out.println(c);

        // Double/Float parsing: format errors must THROW, and the JDK accepts a
        // trailing d/f/D/F and surrounding whitespace.
        try { p("pd_abc", Double.parseDouble("abc")); }
        catch (NumberFormatException e) { p("pd_abc_ex", e.getMessage()); }
        try { p("pd_empty", Double.parseDouble("")); }
        catch (NumberFormatException e) { p("pd_empty_ex", e.getMessage()); }
        p("pd_ws", Double.parseDouble("  1.5  "));
        p("pd_d", Double.parseDouble("1.5d"));
        p("pd_f", Double.parseDouble("1.5f"));
        p("pd_plus", Double.parseDouble("+1.5"));
        p("pd_hex", Double.parseDouble("0x1p3"));
        try { p("pf_abc", Float.parseFloat("abc")); }
        catch (NumberFormatException e) { p("pf_abc_ex", e.getMessage()); }
        p("pd_big", Double.parseDouble("1e400"));
        p("pd_small", Double.parseDouble("1e-400"));
        p("pd_neg0", Double.parseDouble("-0"));

        // Integer.parseInt radix + message shapes
        try { p("pi_r16", Integer.parseInt("ff", 16)); }
        catch (NumberFormatException e) { p("pi_r16_ex", e.getMessage()); }
        try { p("pi_bad16", Integer.parseInt("zz", 16)); }
        catch (NumberFormatException e) { p("pi_bad16_ex", e.getMessage()); }
        try { p("pi_r1", Integer.parseInt("1", 1)); }
        catch (NumberFormatException e) { p("pi_r1_ex", e.getMessage()); }
        try { p("pi_r37", Integer.parseInt("1", 37)); }
        catch (NumberFormatException e) { p("pi_r37_ex", e.getMessage()); }
        try { p("pi_ws", Integer.parseInt(" 1")); }
        catch (NumberFormatException e) { p("pi_ws_ex", e.getMessage()); }
        try { p("pi_plus", Integer.parseInt("+1")); }
        catch (NumberFormatException e) { p("pi_plus_ex", e.getMessage()); }
        try { p("pi_minus", Integer.parseInt("-")); }
        catch (NumberFormatException e) { p("pi_minus_ex", e.getMessage()); }
        try { p("pl_ovf", Long.parseLong("9223372036854775808")); }
        catch (NumberFormatException e) { p("pl_ovf_ex", e.getMessage()); }
        try { p("pui_neg", Integer.parseUnsignedInt("-1")); }
        catch (NumberFormatException e) { p("pui_neg_ex", e.getMessage()); }
        try { p("pui_ovf", Integer.parseUnsignedInt("4294967296")); }
        catch (NumberFormatException e) { p("pui_ovf_ex", e.getMessage()); }
        p("i_decode", Integer.decode("0x1f"));
        p("i_valueOf_s", Integer.valueOf("42"));

        // cross-type equals must be false (different boxed classes)
        p("eq_double_int", Double.valueOf(1.0).equals(Integer.valueOf(1)));
        p("eq_int_int", Integer.valueOf(1).equals(Integer.valueOf(1)));

        System.out.println("DONE");
    }
}
