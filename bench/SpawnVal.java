import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import java.nio.file.*;
import java.util.*;

public class SpawnVal {
    public static void main(String[] a) throws Exception {
        CompoundTag root = NbtIo.readCompressed(Paths.get("world/level.dat"), NbtAccounter.unlimitedHeap());
        CompoundTag data = root.getCompoundOrEmpty("Data");
        System.out.println("SPAWN=" + data.get("spawn"));
        TreeSet<String> t = new TreeSet<>(Arrays.asList("b", "c", "a"));
        System.out.println("TREESET=" + t);
        TreeSet<String> t2 = new TreeSet<>();
        t2.add("z"); t2.add("m"); t2.add("a");
        System.out.println("TREESET2=" + t2);
    }
}
