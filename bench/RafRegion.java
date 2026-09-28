// Faithful repro of MC RegionFile header reading: RandomAccessFile over an .mca,
// read the 4KB offset table (1024 big-endian ints) + a chunk's compressed blob
// header. If rava's raf natives return zeros/garbage, MC sees every chunk as
// absent and silently REGENERATES the whole spawn area (the boot-time worldgen
// grind on a fully cached world).
import java.io.*;

public class RafRegion {
    public static void main(String[] a) throws Exception {
        String path = a.length > 0 ? a[0] : "world/region/r.0.0.mca";
        RandomAccessFile raf = new RandomAccessFile(path, "r");
        long len = raf.length();
        int nonzero = 0, first = -1;
        long acc = 0;
        for (int i = 0; i < 1024; i++) {
            int v = raf.readInt(); // big-endian: (sector<<8)|count
            if (v != 0) { nonzero++; if (first < 0) first = v; }
            acc = acc * 31 + v;
        }
        System.out.println("LEN=" + len);
        System.out.println("NONZERO=" + nonzero);
        System.out.println("FIRST=" + first);
        // Seek to the first chunk's sector and read its 5-byte header
        // (4-byte big-endian payload length + 1-byte compression type).
        if (first > 0) {
            long sector = (first >> 8) & 0xFFFFFF;
            raf.seek(sector * 4096);
            int payload = raf.readInt();
            int ctype = raf.read();
            System.out.println("PAYLOAD=" + payload);
            System.out.println("CTYPE=" + ctype);
            acc = acc * 31 + payload + ctype;
        }
        System.out.println("SUM=" + acc);
        raf.close();
    }
}
