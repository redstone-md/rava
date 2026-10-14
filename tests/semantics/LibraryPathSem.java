import java.io.File;
import java.nio.file.Paths;

// java.library.path is a list in the system's own path syntax: libraries split it on
// File.pathSeparator and resolve each entry (LWJGL looks for its natives that way). On Windows
// HotSpot ends it with the current directory.
public class LibraryPathSem {
    public static void main(String[] args) {
        String path = System.getProperty("java.library.path");
        String[] entries = path.split(File.pathSeparator);
        boolean parse = true;
        for (String e : entries) {
            try {
                Paths.get(e, "lib.dll");
            } catch (RuntimeException ex) {
                parse = false;
                System.out.println("unparsable entry " + e);
            }
        }
        System.out.println("entries parse " + parse);
        System.out.println("windows ends with . " + (!File.separator.equals("\\") || path.endsWith(File.pathSeparator + ".")));
    }
}
