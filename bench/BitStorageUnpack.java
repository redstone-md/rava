// Serial guest-bytecode fixture for SimpleBitStorage.unpack(). It initializes a
// large storage deterministically, unpacks into an int[], then folds the result
// into one FNV hash so the Rava and Java runs can be compared exactly.
import net.minecraft.util.SimpleBitStorage;

public class BitStorageUnpack {
    public static void main(String[] args) {
        int bits = args.length > 0 ? Integer.parseInt(args[0]) : 5;
        int size = args.length > 1 ? Integer.parseInt(args[1]) : 4_000_000;
        int reps = args.length > 2 ? Integer.parseInt(args[2]) : 20;

        SimpleBitStorage storage = new SimpleBitStorage(bits, size);
        int mask = (1 << bits) - 1;
        for (int i = 0; i < size; i++) {
            storage.set(i, (i * 2654435761L & mask) != 0 ? (int) ((i * 31 + 7) & mask) : 0);
        }
        System.out.println("SETUP bits=" + bits + " size=" + size + " reps=" + reps);

        int[] out = new int[size];
        // warm
        storage.unpack(out);
        long t0 = System.nanoTime();
        long hash = 0;
        for (int r = 0; r < reps; r++) {
            storage.unpack(out);
            hash = fold(out); // fold the final unpack
        }
        long ms = (System.nanoTime() - t0) / 1_000_000L;
        System.out.printf("UNPACK reps=%d UNPACK_MS=%d Unpack_HASH=%08x%n", reps, ms, hash & 0xFFFFFFFFL);
    }

    static long fold(int[] a) {
        long h = 1469598103934665603L;
        for (int v : a) {
            for (int s = 0; s < 32; s += 8) {
                h ^= (v >>> s) & 0xFF;
                h *= 1099511628211L;
            }
        }
        return h;
    }
}
