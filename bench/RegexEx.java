// java.util.regex against whatever engine serves it here.
//
// rava's regex is Rust's `regex` crate, which is a DIFFERENT language from
// java.util.regex: no backreferences, no lookaround, and several character
// classes are defined over Unicode where Java's are ASCII-or-Unicode by flag.
// MC runs regex on Identifier validation, command parsing and datapack loading,
// so a class that matches one character more than Java's accepts an id the
// server should have rejected — a silent wrong answer.
//
// Every line prints deterministic text. Diff against `java -Xint`.
import java.util.*;
import java.util.regex.*;

public class RegexEx {
    static void p(String tag, java.util.function.Supplier<Object> s) {
        try { System.out.println(tag + " " + s.get()); }
        catch (Throwable t) { System.out.println(tag + " !" + t.getClass().getName()); }
    }

    static String groups(Matcher m) {
        StringBuilder sb = new StringBuilder();
        while (m.find()) {
            sb.append('[').append(m.start()).append(',').append(m.end()).append(':').append(m.group());
            for (int i = 1; i <= m.groupCount(); i++) sb.append('|').append(m.group(i));
            sb.append(']');
        }
        return sb.length() == 0 ? "-" : sb.toString();
    }

    public static void main(String[] a) {
        // --- classes -------------------------------------------------------------
        p("digit", () -> "a1٣2".replaceAll("\\d", "#"));
        p("word", () -> "a_1-é".replaceAll("\\w", "#"));
        p("space", () -> ("a b\tc d").replaceAll("\\s", "#"));
        p("boundary", () -> "cat cats".replaceAll("\\bcat\\b", "#"));
        p("dot-newline", () -> "a\nb".replaceAll(".", "#"));
        p("dotall", () -> Pattern.compile(".", Pattern.DOTALL).matcher("a\nb").replaceAll("#"));
        p("caret-multiline", () -> Pattern.compile("^b", Pattern.MULTILINE).matcher("a\nb").replaceAll("#"));
        p("posix", () -> "a1 B".replaceAll("\\p{Alpha}", "#"));
        p("negclass", () -> "a1b2".replaceAll("[^0-9]", "#"));
        p("range", () -> "aA0-".replaceAll("[a-zA-Z]", "#"));

        // --- quantifiers and anchors ------------------------------------------------
        p("greedy", () -> "<a><b>".replaceAll("<.+>", "#"));
        p("lazy", () -> "<a><b>".replaceAll("<.+?>", "#"));
        p("possessive", () -> "aaa".matches("a++"));
        p("anchors", () -> "abc".matches("^abc$") + " " + "abc\n".matches("^abc$"));
        p("alt", () -> "cat".replaceAll("cat|dog", "#") + " " + "dog".replaceAll("cat|dog", "#"));

        // --- groups and replacement --------------------------------------------------
        p("group-ref", () -> "john smith".replaceAll("(\\w+) (\\w+)", "$2 $1"));
        p("group-named", () -> Pattern.compile("(?<first>\\w+) (?<second>\\w+)")
            .matcher("john smith").replaceAll("${second} ${first}"));
        p("group-nested", () -> groups(Pattern.compile("((a)(b))+").matcher("abab")));
        p("group-optional", () -> groups(Pattern.compile("a(b)?c").matcher("ac abc")));
        p("replace-dollar-literal", () -> "x".replaceAll("x", Matcher.quoteReplacement("$1")));
        p("replace-backslash", () -> "x".replaceAll("x", "\\\\"));
        p("appendReplacement", () -> {
            Matcher m = Pattern.compile("\\d+").matcher("a1b22c");
            StringBuilder sb = new StringBuilder();
            while (m.find()) m.appendReplacement(sb, "<" + m.group() + ">");
            m.appendTail(sb);
            return sb.toString();
        });

        // --- lookaround and backreferences (Java has them; Rust's crate does not) -----
        p("lookahead", () -> "foo1 foo2".replaceAll("foo(?=1)", "#"));
        p("lookbehind", () -> "1foo 2foo".replaceAll("(?<=1)foo", "#"));
        p("neg-lookahead", () -> "ab ac".replaceAll("a(?!b)", "#"));
        p("backref", () -> "abab abcd".replaceAll("(ab)\\1", "#"));

        // --- split / matches / find --------------------------------------------------
        p("split-limit", () -> Arrays.toString("a:b:c".split(":", 2)) + Arrays.toString("a:b:c".split(":", -1)));
        p("split-empty-pattern", () -> Arrays.toString("abc".split("")) + Arrays.toString(Pattern.compile("").split("abc")));
        p("split-leading", () -> Arrays.toString(":a:b".split(":")) + Arrays.toString(":a:b".split(":", -1)));
        p("split-regex-class", () -> Arrays.toString("a1b22c".split("\\d+")));
        p("pattern-split-limit0", () -> Arrays.toString(Pattern.compile(",").split("a,b,,", 0)));
        p("matches-vs-find", () -> {
            Matcher m = Pattern.compile("b").matcher("abc");
            return m.matches() + " " + m.reset().find() + " " + m.reset().lookingAt();
        });
        p("region-count", () -> {
            Matcher m = Pattern.compile("a").matcher("aaa");
            int n = 0;
            while (m.find()) n++;
            return n + " " + m.groupCount();
        });
        p("start-before-find", () -> Pattern.compile("a").matcher("a").start());
        p("group-before-find", () -> Pattern.compile("a").matcher("a").group());

        // --- compile failures ---------------------------------------------------------
        p("bad-pattern", () -> Pattern.compile("("));
        p("bad-class", () -> Pattern.compile("[z-a]"));
        p("quote", () -> "a.c".replaceAll(Pattern.quote("."), "#"));
        p("flags", () -> Pattern.compile("ABC", Pattern.CASE_INSENSITIVE).matcher("abc").matches()
            + " " + "ABC".matches("(?i)abc"));
        p("pattern-tostring", () -> Pattern.compile("a+b").pattern() + " " + Pattern.compile("a+b"));
    }
}
