// printf/format with the Locale overload — PrintStream.printf(Locale, String, Object...).
// The format string is then args[1], not args[0].
import java.util.Locale;

public class PrintfLocale {
    public static void main(String[] a) {
        System.out.printf(Locale.ROOT, "A|%d|%n", 42);
        System.out.printf(Locale.ROOT, "B|%08x|%n", 48879);
        System.out.printf(Locale.ROOT, "C|%-16s|%n", "left");
        System.out.printf(Locale.ROOT, "D|%.1f|%n", 3.14159);
        System.out.printf(Locale.ROOT, "E|%-16s|%.1f|%n", "both", 2.5);
        System.out.println("F|" + String.format(Locale.ROOT, "%-16s|%.1f", "sfmt", 2.5) + "|");
        System.err.printf(Locale.ROOT, "G|%-16s|%.1f|%n", "err", 2.5);
    }
}
