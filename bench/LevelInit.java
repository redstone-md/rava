// The isInitialized gate: PrimaryLevelData reads Data.initialized through a DFU
// Dynamic over NbtOps. If rava decodes the byte tag as false, EVERY boot re-runs
// setInitialSpawn's expanding findBiomeHorizontal scan (the billions-of-calls
// "Preparing level" churn on an already-initialized world).
import com.mojang.serialization.Dynamic;
import net.minecraft.nbt.*;
import java.nio.file.*;

public class LevelInit {
    public static void main(String[] a) throws Exception {
        CompoundTag root = NbtIo.readCompressed(Paths.get("world/level.dat"), NbtAccounter.unlimitedHeap());
        CompoundTag data = root.getCompoundOrEmpty("Data");
        Dynamic<Tag> d = new Dynamic<>(NbtOps.INSTANCE, data);
        System.out.println("INIT=" + d.get("initialized").asBoolean(false));
        System.out.println("HARDCORE=" + d.get("hardcore").asBoolean(false));
        System.out.println("TIME=" + d.get("Time").asLong(-1));
        System.out.println("NAME=" + d.get("LevelName").asString("?"));
        System.out.println("RAW_INIT=" + data.get("initialized"));
    }
}
