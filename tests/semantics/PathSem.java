// Differential probe: Path arithmetic and the file operations world storage runs on.
// Path manipulation is pure computation; the few real I/O lines use a temp directory.
import java.io.*;
import java.nio.charset.*;
import java.nio.file.*;
import java.util.*;
public class PathSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Callable r) {
        try { r.call(); return "NO THROW"; }
        catch (Throwable t) { return t.getClass().getName(); }
    }
    interface Callable { void call() throws Exception; }
    static String n(Path p) { return p == null ? "null" : p.toString().replace('\\', '/'); }
    public static void main(String[] a) throws Exception {
        // --- pure path arithmetic
        Path base = Paths.get("world", "region");
        p("get", n(base) + " " + base.getNameCount() + " " + n(base.getName(0)) + " " + n(base.getFileName()));
        p("parent", n(base.getParent()) + " " + n(Paths.get("x").getParent()));
        p("resolve", n(base.resolve("r.0.0.mca")) + " " + n(base.resolve(Paths.get("/abs"))));
        p("resolveSibling", n(base.resolveSibling("entities")));
        p("normalize", n(Paths.get("a/./b/../c").normalize()) + " " + n(Paths.get("../x").normalize()));
        p("relativize", n(Paths.get("a/b/c").relativize(Paths.get("a/b/d/e"))));
        p("startsWith", base.startsWith("world") + " " + base.startsWith("wor") + " " + base.endsWith("region"));
        p("subpath", n(Paths.get("a/b/c/d").subpath(1, 3)));
        p("equals", Paths.get("a/b").equals(Paths.get("a", "b")) + " " + Paths.get("a/b").compareTo(Paths.get("a/c")));
        p("empty", n(Paths.get("")) + " " + Paths.get("").getNameCount());
        p("iterate", join(Paths.get("a/b/c")));
        p("absolute", Paths.get("/x/y").isAbsolute() + " " + Paths.get("x/y").isAbsolute());
        p("toUri.scheme", Paths.get("x").toUri().getScheme());
        p("ofString", n(Path.of("a", "b")) + " " + Path.of("a/b").equals(Paths.get("a/b")));

        // --- real files, in a temp directory
        Path dir = Files.createTempDirectory("ravaprobe");
        Path f = dir.resolve("t.txt");
        Files.writeString(f, "line1\nline2\n", StandardCharsets.UTF_8);
        p("exists", Files.exists(f) + " " + Files.isRegularFile(f) + " " + Files.isDirectory(dir) + " " + Files.size(f));
        p("read", Files.readString(f, StandardCharsets.UTF_8).replace("\n", "|"));
        p("lines", Files.readAllLines(f).toString());
        p("bytes", Arrays.toString(Arrays.copyOf(Files.readAllBytes(f), 6)));
        Files.writeString(f, "x", StandardCharsets.UTF_8);
        p("overwrite", Files.readString(f) + " " + Files.size(f));
        Path g = dir.resolve("sub/deep.txt");
        Files.createDirectories(g.getParent());
        Files.writeString(g, "deep");
        p("nested", Files.exists(g) + " " + n(g.getParent().getFileName()));
        try (var s = Files.list(dir)) {
            List<String> names = new ArrayList<>();
            s.forEach(x -> names.add(x.getFileName().toString()));
            Collections.sort(names);
            p("list", names);
        }
        Path copy = dir.resolve("copy.txt");
        Files.copy(f, copy);
        p("copy", Files.readString(copy) + " " + Files.exists(copy));
        p("copyExists", ex(() -> Files.copy(f, copy)));
        Files.move(copy, dir.resolve("moved.txt"));
        p("move", Files.exists(copy) + " " + Files.exists(dir.resolve("moved.txt")));
        p("delete", Files.deleteIfExists(dir.resolve("moved.txt")) + " " + Files.deleteIfExists(dir.resolve("nope.txt")));
        p("deleteMissing", ex(() -> Files.delete(dir.resolve("nope.txt"))));
        p("readMissing", ex(() -> Files.readString(dir.resolve("nope.txt"))));

        // --- encodings, the NBT and packet text path
        byte[] utf8 = "привет ok".getBytes(StandardCharsets.UTF_8);
        p("utf8", utf8.length + " " + new String(utf8, StandardCharsets.UTF_8));
        p("iso", new String("abc".getBytes(StandardCharsets.ISO_8859_1), StandardCharsets.ISO_8859_1));
        p("charset", StandardCharsets.UTF_8.name() + " " + Charset.defaultCharset().name());
        p("badUtf8", new String(new byte[]{ (byte) 0xff, 0x41 }, StandardCharsets.UTF_8).length());

        // --- File, the legacy half
        File file = f.toFile();
        p("File", file.getName() + " " + file.exists() + " " + file.isFile() + " " + file.length());
        p("File.parent", new File("a/b/c.txt").getParent().replace('\\', '/') + " " + new File("c.txt").getParent());
        deleteTree(dir);
    }
    static String join(Path p) { StringBuilder b = new StringBuilder(); for (Path x : p) b.append(x).append(' '); return b.toString().trim(); }
    static void deleteTree(Path d) throws IOException {
        if (!Files.exists(d)) return;
        try (var s = Files.walk(d)) {
            List<Path> all = new ArrayList<>();
            s.forEach(all::add);
            Collections.reverse(all);
            for (Path x : all) Files.deleteIfExists(x);
        }
    }
}
