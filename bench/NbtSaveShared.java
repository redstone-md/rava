// The NbtSave gate's structure builder and canonical hash, factored out so the phase
// decomposition (NbtPhases) runs EXACTLY the gate's work without editing the gate.
import net.minecraft.nbt.*;
import java.util.*;
public class NbtSaveShared {
    public static CompoundTag buildChunk(int c, int sections) {
        CompoundTag chunk = new CompoundTag();
        chunk.putInt("DataVersion", 4082);
        chunk.putInt("xPos", c % 32); chunk.putInt("zPos", c / 32); chunk.putInt("yPos", -4);
        chunk.putString("Status", "minecraft:full");
        chunk.putLong("LastUpdate", 1000L + c); chunk.putLong("InhabitedTime", 0L);
        ListTag sectionList = new ListTag();
        for (int s = 0; s < sections; s++) {
            CompoundTag section = new CompoundTag();
            section.putByte("Y", (byte) (s - 4));
            ListTag palette = new ListTag();
            int variants = 1 + ((c + s) % 8);
            for (int p = 0; p < variants; p++) {
                CompoundTag state = new CompoundTag();
                state.putString("Name", BLOCKS[(c + s + p) % BLOCKS.length]);
                if (p % 3 == 1) {
                    CompoundTag props = new CompoundTag();
                    props.putString("facing", FACINGS[p % FACINGS.length]);
                    props.putString("waterlogged", (p % 2 == 0) ? "true" : "false");
                    state.put("Properties", props);
                }
                palette.add(state);
            }
            CompoundTag blockStates = new CompoundTag();
            blockStates.put("palette", palette);
            int bits = Math.max(4, 32 - Integer.numberOfLeadingZeros(Math.max(1, variants - 1)));
            long[] data = new long[(4096 * bits + 63) / 64];
            for (int i = 0; i < data.length; i++) data[i] = (long) (c * 31 + s * 17 + i) * 6364136223846793005L;
            blockStates.putLongArray("data", data);
            section.put("block_states", blockStates);
            CompoundTag biomes = new CompoundTag();
            ListTag biomePalette = new ListTag();
            for (int b = 0; b < 1 + (s % 3); b++) biomePalette.add(StringTag.valueOf(BIOMES[(c + s + b) % BIOMES.length]));
            biomes.put("palette", biomePalette);
            section.put("biomes", biomes);
            section.putByteArray("BlockLight", light(c, s, 0));
            section.putByteArray("SkyLight", light(c, s, 1));
            sectionList.add(section);
        }
        chunk.put("sections", sectionList);
        CompoundTag heightmaps = new CompoundTag();
        for (String name : HEIGHTMAPS) {
            long[] hm = new long[37];
            for (int i = 0; i < hm.length; i++) hm[i] = (long) (c + i + name.length()) * 1099511628211L;
            heightmaps.putLongArray(name, hm);
        }
        chunk.put("Heightmaps", heightmaps);
        ListTag blockEntities = new ListTag();
        for (int b = 0; b < (c % 5); b++) {
            CompoundTag be = new CompoundTag();
            be.putString("id", "minecraft:chest");
            be.putInt("x", c * 16 + b); be.putInt("y", 60 + b); be.putInt("z", c * 16 - b);
            ListTag items = new ListTag();
            for (int i = 0; i < 3; i++) {
                CompoundTag item = new CompoundTag();
                item.putString("id", BLOCKS[(b + i) % BLOCKS.length]);
                item.putInt("count", 1 + i); item.putByte("Slot", (byte) i);
                items.add(item);
            }
            be.put("Items", items);
            blockEntities.add(be);
        }
        chunk.put("block_entities", blockEntities);
        chunk.put("structures", new CompoundTag());
        return chunk;
    }
    static byte[] light(int c, int s, int kind) {
        byte[] out = new byte[2048];
        for (int i = 0; i < out.length; i++) out[i] = (byte) (c + s * 7 + i * 13 + kind * 3);
        return out;
    }
    public static long canon(Tag t) {
        long h = t.getId();
        if (t instanceof CompoundTag c) {
            for (String k : new TreeSet<>(c.keySet())) {
                h = h * 1000003L + k.hashCode();
                Tag ch = c.get(k);
                h = h * 1000003L + (ch == null ? 0 : canon(ch));
            }
            return h;
        }
        if (t instanceof ListTag l) { for (Tag ch : l) h = h * 1000003L + canon(ch); return h; }
        return h * 1000003L + t.toString().hashCode();
    }
    static final String[] BLOCKS = {"minecraft:stone","minecraft:deepslate","minecraft:dirt","minecraft:grass_block","minecraft:oak_log","minecraft:water","minecraft:air","minecraft:gravel","minecraft:andesite","minecraft:iron_ore"};
    static final String[] FACINGS = {"north","south","east","west"};
    static final String[] BIOMES = {"minecraft:plains","minecraft:forest","minecraft:ocean","minecraft:desert"};
    static final String[] HEIGHTMAPS = {"MOTION_BLOCKING","MOTION_BLOCKING_NO_LEAVES","OCEAN_FLOOR","WORLD_SURFACE"};
}
