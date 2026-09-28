// Block-C decomposition probe: splits the datapack/registry load phase into the
// four layers that could own its wall, each timed independently on the SAME
// 6351-file vanilla data tree that the real boot walks:
//
//   MOUNT   zip FileSystem creation for the server jar's /data root
//   WALK    Files.walk enumeration (nio directory/zip-entry traversal only)
//   READ    Files.newBufferedReader + drain to String (nio read path, no parse)
//   PARSE   StrictJsonParser.parse over ALREADY-READ strings (pure gson/native parse)
//   TOSTR   JsonElement.toString() (tree serialisation; present in DataPackJson's
//           loop but NOT in the real boot, so it must be subtracted from it)
//   CODEC   TagFile.CODEC.parse(JsonOps.INSTANCE, el) over the tag subset — the DFU
//           Codec/MapCodec guest walk that turns a JsonElement into a registry object
//
// The point is that DataPackJson's single PARSE_MS number fuses MOUNT+WALK+READ+
// PARSE+TOSTR, and the real boot additionally pays CODEC on every file. Splitting
// them says which layer to attack.
//
// Deterministic: same FNV-1a fold as DataPackJson so the parse phase remains a
// bit-exact arbiter (PHASES_HASH must match DataPackJson's DataPack_HASH).
import java.io.BufferedReader;
import java.io.StringReader;
import java.net.URI;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.stream.Stream;
import com.google.gson.JsonElement;
import com.mojang.serialization.DataResult;
import com.mojang.serialization.JsonOps;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.tags.TagFile;
import net.minecraft.util.StrictJsonParser;

public class DataPackPhases {
    public static void main(String[] args) throws Exception {
        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        System.out.println("SETUP_MS=" + ms(t0));

        // --- MOUNT -------------------------------------------------------
        long t = System.nanoTime();
        URI uri = DataPackPhases.class.getResource("/data").toURI();
        Path root;
        try {
            root = Paths.get(uri);
        } catch (java.nio.file.FileSystemNotFoundException e) {
            java.nio.file.FileSystems.newFileSystem(uri, java.util.Collections.emptyMap());
            root = Paths.get(uri);
        }
        long mountMs = ms(t);

        // --- WALK --------------------------------------------------------
        t = System.nanoTime();
        List<Path> jsons = new ArrayList<>();
        try (Stream<Path> s = Files.walk(root)) {
            s.forEach(p -> { if (p.toString().endsWith(".json")) jsons.add(p); });
        }
        long walkMs = ms(t);
        jsons.sort((a, b) -> a.toString().compareTo(b.toString()));
        System.out.println("FOUND json files=" + jsons.size());
        System.out.println("SAMPLE_PATH=" + jsons.get(0));

        // --- READ --------------------------------------------------------
        mark("rava.wall.reset"); mark("rava.bridge.reset");
        t = System.nanoTime();
        String[] texts = new String[jsons.size()];
        long bytes = 0;
        int readFail = 0;
        for (int i = 0; i < jsons.size(); i++) {
            try (BufferedReader r = Files.newBufferedReader(jsons.get(i))) {
                StringBuilder sb = new StringBuilder(1024);
                char[] buf = new char[8192];
                int n;
                while ((n = r.read(buf)) > 0) sb.append(buf, 0, n);
                texts[i] = sb.toString();
                bytes += texts[i].length();
            } catch (Exception e) { readFail++; texts[i] = "{}"; }
        }
        long readMs = ms(t);
        mark("rava.wall.dump.READ"); mark("rava.bridge.dump.READ");

        // --- PARSE (pure, from an in-memory StringReader) -----------------
        mark("rava.wall.reset"); mark("rava.bridge.reset");
        t = System.nanoTime();
        JsonElement[] els = new JsonElement[texts.length];
        int parseFail = 0;
        for (int i = 0; i < texts.length; i++) {
            try { els[i] = StrictJsonParser.parse(new StringReader(texts[i])); }
            catch (Exception e) { parseFail++; }
        }
        long parseMs = ms(t);
        mark("rava.wall.dump.PARSE"); mark("rava.bridge.dump.PARSE");

        // --- PARSE (warm second pass) ------------------------------------
        t = System.nanoTime();
        for (int i = 0; i < texts.length; i++) {
            try { StrictJsonParser.parse(new StringReader(texts[i])); } catch (Exception e) {}
        }
        long parse2Ms = ms(t);

        // --- PARSE via the REAL boot path (Files.newBufferedReader) -------
        // The RAVA_GSON_NATIVE hook peeks the reader's underlying $buf, which only exists on
        // the buffered-stream chain; a StringReader misses and falls through to gson bytecode.
        // Timing both arms says whether the native tokenizer is serving these 6351 parses.
        t = System.nanoTime();
        int rpFail = 0;
        for (Path p : jsons) {
            try (BufferedReader r = Files.newBufferedReader(p)) { StrictJsonParser.parse(r); }
            catch (Exception e) { rpFail++; }
        }
        long realParseMs = ms(t);

        // --- TOSTR + hash (the DataPackJson arbiter) ----------------------
        mark("rava.wall.reset"); mark("rava.bridge.reset");
        t = System.nanoTime();
        long hash = 1469598103934665603L;
        long outBytes = 0;
        String[] outs = new String[els.length];
        for (int i = 0; i < els.length; i++) {
            if (els[i] == null) continue;
            outs[i] = els[i].toString();
            outBytes += outs[i].length();
        }
        long tostrMs = ms(t);
        mark("rava.wall.dump.TOSTR"); mark("rava.bridge.dump.TOSTR");

        // --- FOLD (the pure guest char loop, separated from tree serialisation) ---
        mark("rava.wall.reset"); mark("rava.bridge.reset");
        t = System.nanoTime();
        for (int i = 0; i < els.length; i++) {
            if (outs[i] == null) continue;
            hash = fold(hash, jsons.get(i).toString());
            hash = fold(hash, outs[i]);
        }
        long foldMs = ms(t);
        mark("rava.wall.dump.FOLD"); mark("rava.bridge.dump.FOLD");

        // --- CODEC (tag subset, no registry lookups) ----------------------
        List<Integer> tagIdx = new ArrayList<>();
        for (int i = 0; i < jsons.size(); i++) {
            String p = jsons.get(i).toString().replace('\\', '/');
            if (p.contains("/tags/")) tagIdx.add(i);
        }
        int codecOk = 0, codecFail = 0;
        mark("rava.wall.reset"); mark("rava.bridge.reset");
        t = System.nanoTime();
        for (int i : tagIdx) {
            if (els[i] == null) { codecFail++; continue; }
            DataResult<TagFile> dr = TagFile.CODEC.parse(JsonOps.INSTANCE, els[i]);
            if (dr.result().isPresent()) codecOk++; else codecFail++;
        }
        long codecMs = ms(t);
        mark("rava.wall.dump.CODEC"); mark("rava.bridge.dump.CODEC");
        // warm second pass: separates first-run class-load/JIT from steady state
        t = System.nanoTime();
        for (int i : tagIdx) {
            if (els[i] == null) continue;
            TagFile.CODEC.parse(JsonOps.INSTANCE, els[i]);
        }
        long codec2Ms = ms(t);

        // --- CODEC sweep: three more registry-free DIRECT_CODECs -----------
        // TagFile is the simplest possible record (a list of strings), so extrapolating
        // block C's decode cost from it alone would understate. These three are ordinary
        // RecordCodecBuilder records with optional/enum/Component fields and need no
        // registry lookup, so they run standalone and give a per-BYTE decode rate on
        // several codec shapes rather than one.
        codecSweep("damage_type", jsons, els, texts,
            net.minecraft.world.damagesource.DamageType.DIRECT_CODEC);
        codecSweep("banner_pattern", jsons, els, texts,
            net.minecraft.world.level.block.entity.BannerPattern.DIRECT_CODEC);
        codecSweep("chat_type", jsons, els, texts,
            net.minecraft.network.chat.ChatType.DIRECT_CODEC);

        System.out.printf(
            "PHASES files=%d bytes=%d outBytes=%d readFail=%d parseFail=%d "
          + "MOUNT_MS=%d WALK_MS=%d READ_MS=%d PARSE_MS=%d PARSE2_MS=%d REALPARSE_MS=%d rpFail=%d TOSTR_MS=%d FOLD_MS=%d "
          + "tags=%d codecOk=%d codecFail=%d CODEC_MS=%d CODEC2_MS=%d PHASES_HASH=%08x%n",
            jsons.size(), bytes, outBytes, readFail, parseFail,
            mountMs, walkMs, readMs, parseMs, parse2Ms, realParseMs, rpFail, tostrMs, foldMs,
            tagIdx.size(), codecOk, codecFail, codecMs, codec2Ms, hash & 0xFFFFFFFFL);

        // --- top-level directory census (which loader owns the files) -----
        Map<String, int[]> byDir = new TreeMap<>();
        for (int i = 0; i < jsons.size(); i++) {
            String p = jsons.get(i).toString().replace('\\', '/');
            String key = bucket(p);
            int[] v = byDir.computeIfAbsent(key, k -> new int[2]);
            v[0]++;
            v[1] += texts[i].length();
        }
        for (Map.Entry<String, int[]> e : byDir.entrySet()) {
            System.out.printf("DIR %-34s files=%-5d bytes=%d%n",
                e.getKey(), e.getValue()[0], e.getValue()[1]);
        }
    }

    // /data/<ns>/<category>/... -> "<category>" (or the first two segments for tags)
    private static String bucket(String p) {
        int d = p.indexOf("/data/");
        String rest = d >= 0 ? p.substring(d + 6) : p;
        String[] seg = rest.split("/");
        if (seg.length < 2) return rest;
        if (seg.length >= 3 && seg[1].equals("tags")) return "tags/" + seg[2];
        if (seg.length >= 3 && seg[1].equals("worldgen")) return "worldgen/" + seg[2];
        return seg[1];
    }

    // Decode every /data/<ns>/<dir>/ file with `codec` and report ms, ok/fail and bytes,
    // so block C's codec cost can be stated as a per-byte and per-file rate rather than
    // extrapolated from TagFile alone. Cold pass then warm pass.
    private static <T> void codecSweep(String dir, List<Path> jsons, JsonElement[] els,
                                       String[] texts, com.mojang.serialization.Codec<T> codec) {
        List<Integer> idx = new ArrayList<>();
        long inBytes = 0;
        for (int i = 0; i < jsons.size(); i++) {
            String p = jsons.get(i).toString().replace('\\', '/');
            if (p.contains("/" + dir + "/") && !p.contains("/tags/") && els[i] != null) {
                idx.add(i); inBytes += texts[i].length();
            }
        }
        int ok = 0, fail = 0;
        long t = System.nanoTime();
        for (int i : idx) {
            DataResult<T> dr = codec.parse(JsonOps.INSTANCE, els[i]);
            if (dr.result().isPresent()) ok++; else fail++;
        }
        long cold = ms(t);
        t = System.nanoTime();
        for (int r = 0; r < 4; r++) for (int i : idx) codec.parse(JsonOps.INSTANCE, els[i]);
        long warm4 = ms(t);
        System.out.printf("CODECSWEEP %-16s files=%-5d bytes=%-8d ok=%-5d fail=%-4d COLD_MS=%d WARM4_MS=%d%n",
            dir, idx.size(), inBytes, ok, fail, cold, warm4);
    }

    private static long ms(long t0) { return (System.nanoTime() - t0) / 1_000_000L; }

    // Profiler phase bracket. These keys are not real system properties; RAVA intercepts
    // them in System.getProperty and they return null on any JVM, so this is a no-op there.
    private static void mark(String key) { System.getProperty(key); }

    private static long fold(long h, String s) {
        for (int i = 0; i < s.length(); i++) { h ^= (s.charAt(i) & 0xFF); h *= 1099511628211L; }
        h ^= 0x0A;
        h *= 1099511628211L;
        return h;
    }
}
