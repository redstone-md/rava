import java.io.*;
import java.nio.file.*;

// Streams opened to append write at the end of the file, each write: FileWriter and
// FileOutputStream in append mode, opened several times over, and a RandomAccessFile between.
public class FileAppendSem {
    public static void main(String[] args) throws Exception {
        File f = File.createTempFile("rava-append", ".txt");
        try {
            for (int i = 0; i < 3; i++) {
                try (FileWriter w = new FileWriter(f, true)) {
                    w.write("line " + i + "\n");
                }
            }
            try (FileOutputStream o = new FileOutputStream(f, true)) {
                o.write("bytes\n".getBytes());
                o.write('!');
            }
            try (RandomAccessFile r = new RandomAccessFile(f, "rw")) {
                r.seek(0);
                r.write('L');
            }
            try (FileOutputStream o = new FileOutputStream(f, true)) {
                o.write("\nend\n".getBytes());
            }
            System.out.print(new String(Files.readAllBytes(f.toPath())));
            try (FileOutputStream o = new FileOutputStream(f)) {
                o.write("replaced\n".getBytes());
            }
            System.out.print(new String(Files.readAllBytes(f.toPath())));
        } finally {
            f.delete();
        }
    }
}
