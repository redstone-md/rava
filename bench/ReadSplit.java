// Splits the READ layer of the datapack phase into the three things it actually does.
//
// DataPackPhases says READ costs ~1240 ms for 6351 files -- 195 us for a ~780-byte file, against
// the JVM's 19. One number cannot say whether that is finding the entry, decompressing it, or
// turning its bytes into a String, and the three have completely different fixes. Same file list,
// same order, three loops:
//
//   SIZE   Files.size(p)            -- resolve the path, read the central directory. No content.
//   BYTES  Files.readAllBytes(p)    -- adds decompression and a byte[] in the guest heap.
//   TEXT   newBufferedReader+drain  -- adds the stream object, char decoding and StringBuilder.
//
// Each loop is run twice; the second number is the warm one, because the first pays whatever
// one-time cost the archive index has.
import java.io.BufferedReader;
import java.net.URI;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;

public class ReadSplit {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        URI uri = ReadSplit.class.getResource("/data").toURI();
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
        for (int pass = 0; pass < 2; pass++) {
            long sizeSum = 0, byteSum = 0, textSum = 0;

            long t = System.nanoTime();
            for (Path p : files) sizeSum += Files.size(p);
            long sizeMs = ms(t);

            t = System.nanoTime();
            for (Path p : files) byteSum += Files.readAllBytes(p).length;
            long bytesMs = ms(t);

            t = System.nanoTime();
            for (Path p : files) {
                try (BufferedReader r = Files.newBufferedReader(p)) {
                    StringBuilder sb = new StringBuilder(1024);
                    char[] buf = new char[8192];
                    int n;
                    while ((n = r.read(buf)) > 0) sb.append(buf, 0, n);
                    textSum += sb.toString().length();
                }
            }
            long textMs = ms(t);

            System.out.printf("READSPLIT pass=%d files=%d SIZE_MS=%d BYTES_MS=%d TEXT_MS=%d sizeSum=%d byteSum=%d textSum=%d%n",
                    pass, files.size(), sizeMs, bytesMs, textMs, sizeSum, byteSum, textSum);
        }
    }

    private static long ms(long t0) { return (System.nanoTime() - t0) / 1_000_000L; }
}
