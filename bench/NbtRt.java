// Round-trip a small chunk-shaped compound and print a CANONICAL dump (sorted keys,
// one path per line) so a divergence names the exact path rather than a hash.
import net.minecraft.nbt.*;
import java.io.*;
import java.util.*;
public class NbtRt {
    public static void main(String[] a) throws Exception {
        CompoundTag t = new CompoundTag();
        t.putInt("DataVersion", 4082);
        t.putString("Status", "minecraft:full");
        t.putLong("LastUpdate", 1000L);
        ListTag sections = new ListTag();
        CompoundTag s = new CompoundTag();
        s.putByte("Y", (byte) -4);
        ListTag palette = new ListTag();
        CompoundTag st = new CompoundTag();
        st.putString("Name", "minecraft:stone");
        CompoundTag props = new CompoundTag();
        props.putString("facing", "north");
        st.put("Properties", props);
        palette.add(st);
        CompoundTag bs = new CompoundTag();
        bs.put("palette", palette);
        bs.putLongArray("data", new long[]{1L, -2L, 3L});
        s.put("block_states", bs);
        s.putByteArray("BlockLight", new byte[]{1, 2, 3});
        sections.add(s);
        t.put("sections", sections);
        t.putIntArray("ints", new int[]{7, -8});

        ByteArrayOutputStream bo = new ByteArrayOutputStream();
        DataOutputStream out = new DataOutputStream(bo);
        NbtIo.write(t, out);
        out.flush();
        byte[] bytes = bo.toByteArray();
        System.out.println("BYTES=" + bytes.length);
        CompoundTag back = NbtIo.read(new DataInputStream(new ByteArrayInputStream(bytes)),
            NbtAccounter.unlimitedHeap());
        dump("", back);
    }
    static void dump(String path, Tag t) {
        if (t instanceof CompoundTag c) {
            for (String k : new TreeSet<>(c.keySet())) {
                Tag ch = c.get(k);
                System.out.println(path + "/" + k + " id=" + (ch == null ? -1 : ch.getId()));
                if (ch != null) dump(path + "/" + k, ch);
            }
        } else if (t instanceof ListTag l) {
            System.out.println(path + " listsize=" + l.size());
            for (int i = 0; i < l.size(); i++) dump(path + "[" + i + "]", l.get(i));
        } else {
            System.out.println(path + " = " + t);
        }
    }
}
