// level.dat parity: NbtIo.readCompressed the real world/level.dat and print the
// spawn-relevant fields. If rava drops/garbles SpawnX/SpawnY/SpawnZ, MC treats
// the world as fresh and runs the enormous findSpawnBiome climate scan — the
// billions of MultiNoiseBiomeSource/DensityFunctions calls in "Preparing level".
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import java.nio.file.*;
import java.util.*;

public class LevelDat {
    public static void main(String[] a) throws Exception {
        CompoundTag root = NbtIo.readCompressed(Paths.get("world/level.dat"), NbtAccounter.unlimitedHeap());
        CompoundTag data = root.getCompoundOrEmpty("Data");
        System.out.println("KEYS=" + new TreeSet<>(data.keySet()).size());
        System.out.println("SPAWNX=" + data.getIntOr("SpawnX", Integer.MIN_VALUE));
        System.out.println("SPAWNY=" + data.getIntOr("SpawnY", Integer.MIN_VALUE));
        System.out.println("SPAWNZ=" + data.getIntOr("SpawnZ", Integer.MIN_VALUE));
        System.out.println("NAME=" + data.getStringOr("LevelName", "?"));
        System.out.println("TIME=" + data.getLongOr("Time", Long.MIN_VALUE));
        long acc = 0;
        for (String k : new TreeSet<>(data.keySet())) {
            for (int i = 0; i < k.length(); i++) acc = acc * 31 + k.charAt(i);
        }
        System.out.println("KEYSUM=" + acc);
    }
}
