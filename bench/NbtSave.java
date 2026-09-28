// THE SAVE-PATH GATE. Ledger row 263: three product A/Bs agree that the boot to `Done` absorbed
// 82 commits at zero net cost and that the whole regression — ~2.8 s — landed in the phase AFTER
// `Done`, which is the world save and chunk flush. No gate in this tree covered that phase, so the
// cost was invisible to every instrument until a product A/B measured `wall - Done` directly.
//
// This is that instrument: FIXED WORK over the collection-and-string machinery a save actually runs
// — CompoundTag (a map), ListTag (a list), palettes, heightmap long arrays — built, serialised
// through NbtIo and read back. No file I/O: a ByteArrayOutputStream keeps the disk out of the
// number, and the tree already has `wt_boot_ab.ps1` for anything that wants the real thing.
//
// THE HASH IS ORDER-INDEPENDENT ON PURPOSE. A CompoundTag's iteration order is a property of its
// backing map, so hashing the serialised BYTES would gate the map implementation rather than the
// answer, and any change to it would read as a correctness failure. `canon` walks a re-read tag by
// SORTED key instead, which is the same value under any ordering — so the gate stays bit-exact
// across representation changes and still fails loudly if a tag's CONTENT moves.
//
//   arg0 = chunks (default 256), arg1 = sections per chunk (default 24)
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import net.minecraft.nbt.Tag;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeSet;

public class NbtSave {
    public static void main(String[] args) throws Exception {
        int chunks = args.length > 0 ? Integer.parseInt(args[0]) : 256;
        int sections = args.length > 1 ? Integer.parseInt(args[1]) : 24;

        long t0 = System.nanoTime();
        List<CompoundTag> built = new ArrayList<>();
        for (int c = 0; c < chunks; c++) {
            built.add(buildChunk(c, sections));
        }
        long t1 = System.nanoTime();
        System.out.println("BUILD_MS=" + (t1 - t0) / 1_000_000);

        // THE MEASURED PHASE: serialise, read back, fold into the hash.
        long t2 = System.nanoTime();
        long h = 1;
        long bytesTotal = 0;
        for (CompoundTag tag : built) {
            ByteArrayOutputStream sink = new ByteArrayOutputStream(1 << 16);
            DataOutputStream out = new DataOutputStream(sink);
            NbtIo.write(tag, out);
            out.flush();
            byte[] bytes = sink.toByteArray();
            bytesTotal += bytes.length;
            CompoundTag back = NbtIo.read(
                new DataInputStream(new ByteArrayInputStream(bytes)), NbtAccounter.unlimitedHeap());
            h = h * 1000003L + bytes.length;
            h = h * 1000003L + canon(back);
        }
        long t3 = System.nanoTime();
        System.out.println("SAVE_MS=" + (t3 - t2) / 1_000_000
            + " CHUNKS=" + chunks + " SECTIONS=" + sections + " BYTES=" + bytesTotal
            + " NbtSave_HASH=" + Long.toHexString(h));
    }

    /// One chunk's worth of NBT in the shape the real thing has: a section list whose entries carry
    /// a block-state palette and a packed long array, plus the heightmaps and a block-entity list.
    private static CompoundTag buildChunk(int c, int sections) {
        CompoundTag chunk = new CompoundTag();
        chunk.putInt("DataVersion", 4082);
        chunk.putInt("xPos", c % 32);
        chunk.putInt("zPos", c / 32);
        chunk.putInt("yPos", -4);
        chunk.putString("Status", "minecraft:full");
        chunk.putLong("LastUpdate", 1000L + c);
        chunk.putLong("InhabitedTime", 0L);

        ListTag sectionList = new ListTag();
        for (int s = 0; s < sections; s++) {
            CompoundTag section = new CompoundTag();
            section.putByte("Y", (byte) (s - 4));

            // A palette of a handful of states, some with properties — the string-and-map work a
            // real save does most of.
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
            // The packed representation: 4096 cells at the palette's bit width.
            int bits = Math.max(4, 32 - Integer.numberOfLeadingZeros(Math.max(1, variants - 1)));
            long[] data = new long[(4096 * bits + 63) / 64];
            for (int i = 0; i < data.length; i++) {
                data[i] = (long) (c * 31 + s * 17 + i) * 6364136223846793005L;
            }
            blockStates.putLongArray("data", data);
            section.put("block_states", blockStates);

            CompoundTag biomes = new CompoundTag();
            ListTag biomePalette = new ListTag();
            for (int b = 0; b < 1 + (s % 3); b++) {
                biomePalette.add(net.minecraft.nbt.StringTag.valueOf(BIOMES[(c + s + b) % BIOMES.length]));
            }
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
            for (int i = 0; i < hm.length; i++) {
                hm[i] = (long) (c + i + name.length()) * 1099511628211L;
            }
            heightmaps.putLongArray(name, hm);
        }
        chunk.put("Heightmaps", heightmaps);

        ListTag blockEntities = new ListTag();
        for (int b = 0; b < (c % 5); b++) {
            CompoundTag be = new CompoundTag();
            be.putString("id", "minecraft:chest");
            be.putInt("x", c * 16 + b);
            be.putInt("y", 60 + b);
            be.putInt("z", c * 16 - b);
            ListTag items = new ListTag();
            for (int i = 0; i < 3; i++) {
                CompoundTag item = new CompoundTag();
                item.putString("id", BLOCKS[(b + i) % BLOCKS.length]);
                item.putInt("count", 1 + i);
                item.putByte("Slot", (byte) i);
                items.add(item);
            }
            be.put("Items", items);
            blockEntities.add(be);
        }
        chunk.put("block_entities", blockEntities);
        chunk.put("structures", new CompoundTag());
        return chunk;
    }

    private static byte[] light(int c, int s, int kind) {
        byte[] out = new byte[2048];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) (c + s * 7 + i * 13 + kind * 3);
        }
        return out;
    }

    /// Value hash that does not depend on map iteration order: compounds are walked by SORTED key,
    /// everything else in its own order, and leaves fold their `toString()` — which NBT defines and
    /// which therefore carries the CONTENT this gate is protecting.
    private static long canon(Tag t) {
        long h = t.getId();
        if (t instanceof CompoundTag ct) {
            for (String k : new TreeSet<>(ct.keySet())) {
                h = h * 1000003L + k.hashCode();
                Tag child = ct.get(k);
                h = h * 1000003L + (child == null ? 0 : canon(child));
            }
            return h;
        }
        if (t instanceof ListTag lt) {
            for (Tag child : lt) {
                h = h * 1000003L + canon(child);
            }
            return h;
        }
        return h * 1000003L + t.toString().hashCode();
    }

    private static final String[] BLOCKS = {
        "minecraft:stone", "minecraft:deepslate", "minecraft:dirt", "minecraft:grass_block",
        "minecraft:oak_log", "minecraft:water", "minecraft:air", "minecraft:gravel",
        "minecraft:andesite", "minecraft:iron_ore",
    };
    private static final String[] FACINGS = { "north", "south", "east", "west" };
    private static final String[] BIOMES = {
        "minecraft:plains", "minecraft:forest", "minecraft:ocean", "minecraft:desert",
    };
    private static final String[] HEIGHTMAPS = {
        "MOTION_BLOCKING", "MOTION_BLOCKING_NO_LEAVES", "OCEAN_FLOOR", "WORLD_SURFACE",
    };
}
