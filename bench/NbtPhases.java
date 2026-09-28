// WHERE THE SAVE TIME GOES. Same work as the NbtSave gate, timed per phase, so the
// decomposition needs no sampler — the gate itself stays untouched and keeps its hash.
import net.minecraft.nbt.*;
import java.io.*;
import java.util.*;
public class NbtPhases {
    public static void main(String[] args) throws Exception {
        int chunks = args.length > 0 ? Integer.parseInt(args[0]) : 64;
        int sections = args.length > 1 ? Integer.parseInt(args[1]) : 24;
        List<CompoundTag> built = new ArrayList<>();
        long t0 = System.nanoTime();
        for (int c = 0; c < chunks; c++) built.add(NbtSaveShared.buildChunk(c, sections));
        long t1 = System.nanoTime();

        long write = 0, read = 0, canon = 0, bytesTotal = 0;
        List<byte[]> blobs = new ArrayList<>();
        for (CompoundTag tag : built) {
            long a = System.nanoTime();
            ByteArrayOutputStream sink = new ByteArrayOutputStream(1 << 16);
            DataOutputStream out = new DataOutputStream(sink);
            NbtIo.write(tag, out);
            out.flush();
            byte[] bytes = sink.toByteArray();
            long b = System.nanoTime();
            blobs.add(bytes);
            bytesTotal += bytes.length;
            write += b - a;
        }
        List<CompoundTag> backs = new ArrayList<>();
        for (byte[] bytes : blobs) {
            long a = System.nanoTime();
            backs.add(NbtIo.read(new DataInputStream(new ByteArrayInputStream(bytes)),
                NbtAccounter.unlimitedHeap()));
            read += System.nanoTime() - a;
        }
        long h = 1;
        for (CompoundTag back : backs) {
            long a = System.nanoTime();
            h = h * 1000003L + NbtSaveShared.canon(back);
            canon += System.nanoTime() - a;
        }
        System.out.println("BUILD_MS=" + (t1 - t0) / 1_000_000
            + " WRITE_MS=" + write / 1_000_000
            + " READ_MS=" + read / 1_000_000
            + " CANON_MS=" + canon / 1_000_000
            + " BYTES=" + bytesTotal + " H=" + Long.toHexString(h));
    }
}
