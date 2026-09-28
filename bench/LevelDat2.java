import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import java.nio.file.*;
import java.util.*;

public class LevelDat2 {
    public static void main(String[] a) throws Exception {
        CompoundTag root = NbtIo.readCompressed(Paths.get("world/level.dat"), NbtAccounter.unlimitedHeap());
        CompoundTag data = root.getCompoundOrEmpty("Data");
        for (String k : new TreeSet<>(data.keySet())) System.out.println("K=" + k);
    }
}
