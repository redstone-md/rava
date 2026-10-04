import java.nio.file.*;
import java.util.concurrent.TimeUnit;

/** java.nio.file.WatchService (inotify on Linux): a file created in a watched directory is
 *  reported as ENTRY_CREATE with its name. */
public class WatchServiceSem {
    public static void main(String[] args) throws Exception {
        Path dir = Files.createTempDirectory("watch");
        try (WatchService ws = FileSystems.getDefault().newWatchService()) {
            dir.register(ws, StandardWatchEventKinds.ENTRY_CREATE);
            Files.writeString(dir.resolve("made.txt"), "x");
            WatchKey key = ws.poll(10, TimeUnit.SECONDS);
            System.out.println(key != null);
            for (WatchEvent<?> e : key.pollEvents()) System.out.println(e.kind() + " " + e.context());
            System.out.println(key.reset());
        } finally {
            Files.deleteIfExists(dir.resolve("made.txt"));
            Files.delete(dir);
        }
    }
}
