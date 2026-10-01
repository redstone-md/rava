// Differential probe: enums, EnumMap/EnumSet, UUID, Base64, regex — the platform pieces
// this jar leans on (directions, player ids, skins, command parsing).
//
// ONE LINE IS EXPECTED TO DIVERGE, and it is a deliberate limit rather than a defect:
//
//   re.bad  the JVM raises PatternSyntaxException for a malformed pattern; this engine
//           does not raise at all. It CANNOT tell "malformed" from "valid Java this
//           engine's regex crate does not implement" — log4j compiles a pattern with a
//           lookahead, which is correct Java and unsupported here, and raising on it
//           killed the server before its first log line. Silently failing to match is
//           wrong for the first case and right for the second, and the second is the one
//           that has to keep working.
// Machine-readable form of the note above, for bench\sem_all.ps1. PatternSyntaxException
// renders over THREE lines (message, the offending pattern, a caret), so the two
// continuation lines are declared as well - they exist only on the JVM side, and only
// because the exception this engine deliberately does not raise has a multi-line toString.
// DIVERGES: re.bad
// DIVERGES: [
// DIVERGES: ^
import java.util.*;
import java.util.regex.*;
public class PlatSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName() + ": " + t.getMessage(); }
    }
    enum Dir { NORTH, EAST, SOUTH, WEST;
        Dir opposite() { return values()[(ordinal() + 2) % 4]; }
    }
    enum Sized { SMALL(1), BIG(10);
        final int n; Sized(int n) { this.n = n; }
        int n() { return n; }
    }
    public static void main(String[] a) {
        // --- enum basics
        p("enum.values", Arrays.toString(Dir.values()));
        p("enum.name", Dir.EAST.name() + " " + Dir.EAST.ordinal() + " " + Dir.EAST);
        p("enum.valueOf", Dir.valueOf("SOUTH") + " " + (Dir.valueOf("SOUTH") == Dir.SOUTH));
        p("enum.bad", ex(() -> Dir.valueOf("UP")));
        p("enum.compare", Dir.NORTH.compareTo(Dir.WEST) + " " + Dir.WEST.compareTo(Dir.NORTH));
        p("enum.method", Dir.NORTH.opposite() + " " + Sized.BIG.n());
        p("enum.equals", Dir.NORTH.equals(Dir.NORTH) + " " + (Dir.NORTH == Dir.valueOf("NORTH")));
        p("enum.switch", sw(Dir.EAST) + " " + sw(Dir.WEST));
        p("enum.getClass", Dir.NORTH.getDeclaringClass().getSimpleName());

        // --- EnumMap / EnumSet: ordinal order, not insertion
        EnumMap<Dir, Integer> em = new EnumMap<>(Dir.class);
        em.put(Dir.WEST, 4); em.put(Dir.NORTH, 1); em.put(Dir.SOUTH, 3);
        p("enummap", em);
        p("enummap.keys", em.keySet());
        p("enumset.of", EnumSet.of(Dir.WEST, Dir.NORTH));
        p("enumset.all", EnumSet.allOf(Dir.class));
        p("enumset.none", EnumSet.noneOf(Dir.class).size());
        p("enumset.range", EnumSet.range(Dir.EAST, Dir.WEST));
        p("enumset.complement", EnumSet.complementOf(EnumSet.of(Dir.NORTH, Dir.EAST)));

        // --- UUID
        UUID u = UUID.fromString("069a79f4-44e9-4726-a5be-fca90e38aaf5");
        p("uuid", u + " " + u.getMostSignificantBits() + " " + u.getLeastSignificantBits());
        p("uuid.version", u.version() + " " + u.variant());
        p("uuid.eq", u.equals(UUID.fromString("069a79f4-44e9-4726-a5be-fca90e38aaf5")) + " " + (u.hashCode() == UUID.fromString("069a79f4-44e9-4726-a5be-fca90e38aaf5").hashCode()));
        p("uuid.bits", new UUID(1L, 2L).toString());
        p("uuid.bad", ex(() -> UUID.fromString("nope")));
        p("uuid.nameBased", UUID.nameUUIDFromBytes("OfflinePlayer:Notch".getBytes()).toString());

        // --- Base64
        String enc = Base64.getEncoder().encodeToString("hello world".getBytes());
        p("b64", enc + " " + new String(Base64.getDecoder().decode(enc)));
        p("b64.pad", Base64.getEncoder().encodeToString("a".getBytes()) + " " + Base64.getEncoder().encodeToString("ab".getBytes()));
        p("b64.url", Base64.getUrlEncoder().encodeToString(new byte[]{ (byte)0xfb, (byte)0xff }));
        p("b64.nopad", Base64.getEncoder().withoutPadding().encodeToString("a".getBytes()));
        p("b64.bad", ex(() -> Base64.getDecoder().decode("!!!")));

        // --- regex
        Pattern pat = Pattern.compile("(\\w+)@(\\w+)\\.com");
        Matcher m = pat.matcher("write to bob@example.com now");
        p("re.find", m.find() + " " + m.group() + " " + m.group(1) + " " + m.group(2));
        p("re.pos", m.start() + " " + m.end());
        p("re.matches", Pattern.matches("[a-z]+", "abc") + " " + Pattern.matches("[a-z]+", "aBc"));
        p("re.replace", "a1b2c3".replaceAll("\\d", "#") + " " + "a1b2".replaceFirst("\\d", "#"));
        p("re.split", Arrays.toString(Pattern.compile("\\s*,\\s*").split("a , b,c")));
        p("re.groupCount", pat.matcher("x").groupCount());
        p("re.named", named());
        p("re.quote", Pattern.quote("a.b") + " " + "a.b".matches(Pattern.quote("a.b")));
        p("re.bad", ex(() -> Pattern.compile("[")));
        p("re.noMatchGroup", ex(() -> { Matcher mm = pat.matcher("zzz"); mm.group(); }));
        p("re.lookingAt", pat.matcher("bob@example.com tail").lookingAt());
        p("re.appendReplacement", appendRepl());
    }
    static String sw(Dir d) {
        return switch (d) { case NORTH -> "n"; case EAST -> "e"; default -> "other"; };
    }
    static String named() {
        Matcher m = Pattern.compile("(?<user>\\w+)@(?<host>\\w+)").matcher("bob@host");
        return m.find() ? m.group("user") + "/" + m.group("host") : "-";
    }
    static String appendRepl() {
        Matcher m = Pattern.compile("\\d").matcher("a1b2");
        StringBuilder sb = new StringBuilder();
        while (m.find()) m.appendReplacement(sb, "[" + m.group() + "]");
        m.appendTail(sb);
        return sb.toString();
    }
}
