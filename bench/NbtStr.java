// Which NBT tag renders differently? canon() in NbtSave folds leaf toString(), so a
// divergence there is a divergence in guest-visible text.
import net.minecraft.nbt.*;
public class NbtStr {
    public static void main(String[] a) {
        p("byte", ByteTag.valueOf((byte) -3));
        p("short", ShortTag.valueOf((short) 300));
        p("int", IntTag.valueOf(-7));
        p("long", LongTag.valueOf(1234567890123L));
        p("float", FloatTag.valueOf(1.5f));
        p("float2", FloatTag.valueOf(0.1f));
        p("double", DoubleTag.valueOf(2.25));
        p("double2", DoubleTag.valueOf(0.1));
        p("string", StringTag.valueOf("hello"));
        p("string-quote", StringTag.valueOf("a\"b"));
        p("bytearr", new ByteArrayTag(new byte[]{1,-2,3}));
        p("intarr", new IntArrayTag(new int[]{1,-2,3}));
        p("longarr", new LongArrayTag(new long[]{1L,-2L,3L}));
        ListTag l = new ListTag(); l.add(IntTag.valueOf(1)); l.add(IntTag.valueOf(2));
        p("list", l);
        CompoundTag c = new CompoundTag();
        c.putInt("i", 1); c.putString("s", "x"); c.putLongArray("la", new long[]{9L});
        p("compound", c);
        System.out.println("ids byte=" + ByteTag.valueOf((byte)1).getId()
            + " long=" + LongTag.valueOf(1L).getId()
            + " larr=" + new LongArrayTag(new long[]{1L}).getId()
            + " list=" + l.getId() + " comp=" + c.getId());
    }
    static void p(String n, Tag t) { System.out.println(n + " -> " + t.toString()); }
}
