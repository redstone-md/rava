import java.io.*;
import java.nio.MappedByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.file.*;
import java.util.prefs.Preferences;

// Class library features that end in a platform native: Java serialization (the default
// serialVersionUID asks whether a class has a static initializer), a mapped buffer's load and
// force, a file's content type (the registry on Windows) and preferences (the registry on
// Windows, files elsewhere).
public class PlatformNativesSem {
    static class Point implements Serializable {
        static int made;
        int x = 3, y = 4;
    }

    static class Plain implements Serializable {
        int z = 5;
    }

    public static void main(String[] args) throws Exception {
        java.util.logging.Logger.getLogger("java.util.prefs").setLevel(java.util.logging.Level.OFF);
        ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        try (ObjectOutputStream out = new ObjectOutputStream(bytes)) {
            out.writeObject(new Point());
            out.writeObject(new Plain());
        }
        try (ObjectInputStream in = new ObjectInputStream(new ByteArrayInputStream(bytes.toByteArray()))) {
            Point p = (Point) in.readObject();
            Plain q = (Plain) in.readObject();
            System.out.println("serialized " + p.x + " " + p.y + " " + q.z);
        }
        System.out.println("suid " + ObjectStreamClass.lookup(Point.class).getSerialVersionUID() + " " + ObjectStreamClass.lookup(Plain.class).getSerialVersionUID());

        Path file = Files.createTempFile("rava-probe", ".txt");
        var unsafeField = sun.misc.Unsafe.class.getDeclaredField("theUnsafe");
        unsafeField.setAccessible(true);
        sun.misc.Unsafe unsafe = (sun.misc.Unsafe) unsafeField.get(null);
        try {
            Files.write(file, new byte[8192]);
            try (FileChannel ch = FileChannel.open(file, StandardOpenOption.READ, StandardOpenOption.WRITE)) {
                MappedByteBuffer m = ch.map(FileChannel.MapMode.READ_WRITE, 0, 8192);
                try {
                    m.put(100, (byte) 42);
                    m.load();
                    m.isLoaded();
                    m.force();
                } finally {
                    // Windows cannot delete a file while a mapped section still holds it.
                    unsafe.invokeCleaner(m);
                }
            }
            System.out.println("mapped " + Files.readAllBytes(file)[100]);
            System.out.println("content type " + Files.probeContentType(file));
        } finally {
            Files.deleteIfExists(file);
        }

        Preferences node = Preferences.userRoot().node("rava-semantics-probe");
        node.put("answer", "42");
        node.putInt("count", 7);
        System.out.println("prefs " + node.get("answer", "none") + " " + node.getInt("count", 0) + " " + node.keys().length);
        node.removeNode();
        Preferences.userRoot().flush();
        System.out.println("prefs removed " + Preferences.userRoot().nodeExists("rava-semantics-probe"));
    }
}
