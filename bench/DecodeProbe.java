// Step-2 measurement probe: run the REAL DFU decode pipeline (StrictJsonParser.parse →
// Codec.parse(JsonOps.INSTANCE, jsonElement) → final object) over every vanilla *tag* JSON.
// Tags use TagFile.CODEC, a RecordCodecBuilder codec that needs NO registry lookup, so it is
// self-contained yet exercises the full generic stack: gson tree build, JsonOps leaf access,
// and the DataResult/Pair/Optional monad walk. Bracketed by the RAVA_ALLOC_HISTO sentinels
// (System.getProperty rava.histo.reset / rava.histo.dump) so the Rust side prints the allocation
// composition — gson-tree storm vs dfu-monad storm vs final/domain objects — which is exactly
// the A-vs-C scoping decision for Step-2.
//
// Deterministic: decode results folded (path + TagFile.toString) into one FNV hash so a later
// native mapper can be proven bit-exact against this baseline.
import java.io.BufferedReader;
import java.net.URI;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;
import com.google.gson.JsonElement;
import com.mojang.serialization.DataResult;
import com.mojang.serialization.JsonOps;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.tags.TagFile;
import net.minecraft.util.StrictJsonParser;

public class DecodeProbe {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        URI uri = DecodeProbe.class.getResource("/data").toURI();
        Path root;
        try {
            root = Paths.get(uri);
        } catch (java.nio.file.FileSystemNotFoundException e) {
            // Real JVM: a jar: URI needs its zip FileSystem mounted first (Rava auto-mounts).
            java.nio.file.FileSystems.newFileSystem(uri, java.util.Collections.emptyMap());
            root = Paths.get(uri);
        }
        List<Path> tags = new ArrayList<>();
        try (Stream<Path> s = Files.walk(root)) {
            s.forEach(p -> {
                String t = p.toString().replace('\\', '/');
                if (t.endsWith(".json") && t.contains("/tags/")) tags.add(p);
            });
        }
        tags.sort((a, b) -> a.toString().compareTo(b.toString()));
        System.out.println("TAG files=" + tags.size());

        // Warm (unmeasured): force class-load + JIT of the whole codec path.
        Res warm = run(tags);
        System.out.println("WARM tags=" + warm.count + " fail=" + warm.fail);

        // Measured phase, allocation-bracketed.
        System.getProperty("rava.histo.reset");
        long t0 = System.nanoTime();
        Res r = run(tags);
        long ms = (System.nanoTime() - t0) / 1_000_000L;
        System.getProperty("rava.histo.dump");
        System.out.printf("DECODE tags=%d fail=%d DECODE_MS=%d Decode_HASH=%08x%n",
            r.count, r.fail, ms, r.hash & 0xFFFFFFFFL);
    }

    static Res run(List<Path> tags) throws Exception {
        long h = 1469598103934665603L;
        int count = 0, fail = 0;
        for (Path p : tags) {
            try (BufferedReader br = Files.newBufferedReader(p)) {
                JsonElement el = StrictJsonParser.parse(br);
                DataResult<TagFile> dr = TagFile.CODEC.parse(JsonOps.INSTANCE, el);
                if (dr.result().isPresent()) {
                    h = fold(h, p.toString());
                    h = fold(h, dr.result().get().toString());
                    count++;
                } else {
                    fail++;
                }
            } catch (Exception e) {
                fail++;
            }
        }
        return new Res(h, count, fail);
    }

    static long fold(long h, String s) {
        for (int i = 0; i < s.length(); i++) { h ^= (s.charAt(i) & 0xFF); h *= 1099511628211L; }
        h ^= 0x0A; h *= 1099511628211L;
        return h;
    }

    static final class Res {
        final long hash; final int count, fail;
        Res(long hash, int count, int fail) { this.hash = hash; this.count = count; this.fail = fail; }
    }
}
