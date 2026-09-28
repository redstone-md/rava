// Names the FIRST file where rava and the JVM disagree on the datapack walk.
//
// ANSWER, 2026-08-30: the parse is bit-identical. All 6351 per-file content hashes match
// exactly; the ONLY difference is the path rendering. A zip-filesystem Path renders as
// "/data/minecraft/..." on the JVM and "data/minecraft/..." here -- the leading slash of an
// ABSOLUTE zip path is missing. That is what makes DataPackJson's single fused hash differ
// between the engines (14e8ee5a vs 7b611053), and it is a path-rendering defect, not a JSON one.
//
// Not fixed yet, deliberately: jar entries are stored slash-less and `resolve`/`startsWith`/
// `relativize`/the class loader all read that same string, so rendering absolute paths correctly
// means giving a jar path an absoluteness flag rather than prepending a character. Recorded here
// so the next reader does not re-derive it from one mismatched hash.
//
// DataPackJson folds `p.toString()` AND the parsed `JsonElement.toString()` into one hash, and
// the two engines report different hashes for the same 6351 files. One number cannot say which
// half moved, or where. This prints one line per file -- the path, and a per-file fold of the
// parsed text alone -- so `diff` names the first disagreement and says whether it is the PATH
// rendering (a probe artifact: zip-filesystem Path.toString) or the PARSE (a real defect on the
// phase worth 26% of the boot).
import java.io.BufferedReader;
import java.net.URI;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.stream.Stream;
import com.google.gson.JsonElement;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.StrictJsonParser;

public class DataPackDiff {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        URI uri = DataPackDiff.class.getResource("/data").toURI();
        Path root;
        if ("jar".equals(uri.getScheme())) {
            String s = uri.toString();
            int bang = s.indexOf("!/");
            root = FileSystems.newFileSystem(URI.create(s.substring(0, bang)), new java.util.HashMap<String, Object>())
                    .getPath(s.substring(bang + 1));
        } else {
            root = Paths.get(uri);
        }
        List<Path> files = new ArrayList<>();
        try (Stream<Path> st = Files.walk(root)) {
            for (Path p : (Iterable<Path>) st::iterator) {
                if (p.toString().endsWith(".json")) files.add(p);
            }
        }
        List<String> keys = new ArrayList<>();
        for (Path p : files) keys.add(p.toString());
        Collections.sort(keys);
        // Re-resolve by string key so BOTH engines walk the same order regardless of how their
        // Files.walk enumerates -- otherwise a pure ORDER difference would masquerade as content.
        for (String k : keys) {
            Path p = root.getFileSystem().getPath(k);
            String out;
            try (BufferedReader r = Files.newBufferedReader(p)) {
                JsonElement el = StrictJsonParser.parse(r);
                out = String.format("%08x", fold(0xcbf29ce484222325L, el.toString()) & 0xFFFFFFFFL);
            } catch (Exception e) {
                out = "FAIL:" + e.getClass().getName();
            }
            System.out.println(k + "\t" + out);
        }
    }

    private static long fold(long h, String s) {
        for (int i = 0; i < s.length(); i++) { h ^= (s.charAt(i) & 0xFF); h *= 1099511628211L; }
        return h;
    }
}
