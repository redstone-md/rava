// RegionFile's real I/O shape in plain Java: FileChannel + ByteBuffer positioned
// reads of the .mca header + first chunk blob, then an InflaterInputStream over
// the deflate payload (compression type 2) — exactly what MC does in
// RegionFile.getChunkDataInputStream. Compares byte-exact under JVM vs rava.
import java.io.*;
import java.nio.*;
import java.nio.channels.*;
import java.nio.file.*;
import java.util.zip.*;

public class ChanRegion {
    public static void main(String[] a) throws Exception {
        Path p = Paths.get(a.length > 0 ? a[0] : "world/region/r.0.0.mca");
        FileChannel ch = FileChannel.open(p, StandardOpenOption.READ);
        System.out.println("CHLEN=" + ch.size());

        ByteBuffer header = ByteBuffer.allocate(8192);
        ch.read(header, 0);
        header.flip();
        IntBuffer offsets = header.asIntBuffer();
        int nonzero = 0, first = -1;
        long acc = 0;
        for (int i = 0; i < 1024; i++) {
            int v = offsets.get(i);
            if (v != 0) { nonzero++; if (first < 0) first = v; }
            acc = acc * 31 + v;
        }
        System.out.println("NONZERO=" + nonzero);
        System.out.println("FIRST=" + first);

        if (first > 0) {
            long sector = (first >> 8) & 0xFFFFFF;
            ByteBuffer head = ByteBuffer.allocate(5);
            ch.read(head, sector * 4096);
            head.flip();
            int payload = head.getInt();
            int ctype = head.get() & 0xFF;
            System.out.println("PAYLOAD=" + payload + " CTYPE=" + ctype);
            ByteBuffer blob = ByteBuffer.allocate(payload - 1);
            ch.read(blob, sector * 4096 + 5);
            blob.flip();
            byte[] raw = new byte[blob.remaining()];
            blob.get(raw);
            // ctype 2 = zlib deflate — the NBT root should begin 0x0A ("compound").
            InflaterInputStream in = new InflaterInputStream(new ByteArrayInputStream(raw));
            int b, n = 0;
            while (n < 4096 && (b = in.read()) >= 0) { acc = acc * 31 + b; n++; }
            in.close();
            System.out.println("NBT_BYTES=" + n);
        }
        ch.close();
        System.out.println("SUM=" + acc);
    }
}
