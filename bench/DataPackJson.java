// Standalone repro of the datapack-JSON reload phase of server boot: walk the
// built-in vanilla data tree (/data/**) inside the server jar and parse every
// *.json through net.minecraft.util.StrictJsonParser.parse(Reader) — the exact
// call the real loaders use (SimpleJsonResourceReloadListener.scanDirectory,
// RegistryDataLoader.loadElementFromResource, TagLoader). This is the path the
// RAVA_GSON_NATIVE hook intercepts; a code-bootstrap worldgen bench (RingScan)
// never reaches it because it uses VanillaRegistries.createLookup() instead.
//
// Enumeration is generic: getResource("/data") → jar FileSystem → Files.walk,
// exactly how VanillaPackResources discovers its roots — no hard-coded file list
// and nothing specific to this jar.
//
// Deterministic: files are parsed in sorted path order and the canonical
// JsonElement.toString() of each is folded into one 64-bit FNV hash. The hash is
// identical whether the tree came from the Rust tokenizer (native ON) or the gson
// JsonReader bytecode (native OFF) iff they produce the same tree — a bit-exact
// arbiter for the intrinsic. Comparable between JVM and rava.
import java.io.BufferedReader;
import java.net.URI;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;
import com.google.gson.JsonElement;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.StrictJsonParser;

public class DataPackJson {
    public static void main(String[] args) throws Exception {
        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long setupMs = (System.nanoTime() - t0) / 1_000_000L;
        System.out.println("SETUP_MS=" + setupMs);

        URI uri = DataPackJson.class.getResource("/data").toURI();
        Path root;
        try {
            root = Paths.get(uri);
        } catch (java.nio.file.FileSystemNotFoundException e) {
            // Real JVM: a jar: URI needs its zip FileSystem mounted first (Rava auto-mounts).
            java.nio.file.FileSystems.newFileSystem(uri, java.util.Collections.emptyMap());
            root = Paths.get(uri);
        }
        List<Path> jsons = new ArrayList<>();
        try (Stream<Path> s = Files.walk(root)) {
            s.forEach(p -> { if (p.toString().endsWith(".json")) jsons.add(p); });
        }
        jsons.sort((a, b) -> a.toString().compareTo(b.toString()));
        System.out.println("FOUND json files=" + jsons.size());

        long t1 = System.nanoTime();
        long hash = 1469598103934665603L; // FNV-1a 64 offset basis
        int count = 0, fail = 0;
        long bytes = 0;
        for (Path p : jsons) {
            try (BufferedReader r = Files.newBufferedReader(p)) {
                JsonElement el = StrictJsonParser.parse(r);
                String s = el.toString();
                bytes += s.length();
                hash = fold(hash, p.toString());
                hash = fold(hash, s);
                count++;
            } catch (Exception e) {
                fail++;
                if (fail <= 8) System.out.println("PARSE_FAIL " + p + " : " + e);
            }
        }
        long parseMs = (System.nanoTime() - t1) / 1_000_000L;
        System.out.printf(
            "DATAPACK count=%d fail=%d bytes=%d PARSE_MS=%d DataPack_HASH=%08x%n",
            count, fail, bytes, parseMs, hash & 0xFFFFFFFFL);
    }

    // FNV-1a 64 over the char stream (ASCII JSON), order-sensitive.
    private static long fold(long h, String s) {
        for (int i = 0; i < s.length(); i++) {
            h ^= (s.charAt(i) & 0xFF);
            h *= 1099511628211L;
        }
        h ^= 0x0A; // record separator so concatenation can't alias
        h *= 1099511628211L;
        return h;
    }
}
