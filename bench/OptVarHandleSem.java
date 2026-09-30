import java.lang.invoke.*;

/** VarHandle access through constant handles, folded down to field and element accesses
 *  (stable member-name tables, merges with a pruned side): volatile, opaque, release and
 *  compare-and-set on fields and array elements, and the failures they report. */
public class OptVarHandleSem {
    volatile Object ref; int n; long big;
    static final VarHandle REF, N, BIG;
    static final VarHandle AA = MethodHandles.arrayElementVarHandle(Object[].class);
    static final VarHandle IA = MethodHandles.arrayElementVarHandle(int[].class);
    static {
        try {
            var l = MethodHandles.lookup();
            REF = l.findVarHandle(OptVarHandleSem.class, "ref", Object.class);
            N = l.findVarHandle(OptVarHandleSem.class, "n", int.class);
            BIG = l.findVarHandle(OptVarHandleSem.class, "big", long.class);
        } catch (Exception e) { throw new Error(e); }
    }

    static long run(OptVarHandleSem v, Object[] arr, int[] ints, int k) {
        long s = 0;
        for (int i = 0; i < k; i++) {
            Object o = REF.getVolatile(v);
            s += (o == null ? 0 : 1) + (int) N.getOpaque(v);
            N.setRelease(v, i);
            s += AA.getVolatile(arr, i & 7) == null ? 0 : 1;
            IA.getAndAdd(ints, i & 3, 1);
            if (REF.compareAndSet(v, o, (i & 1) == 0 ? "a" : "b")) s++;
            BIG.getAndAdd(v, (long) i);
        }
        return s;
    }

    public static void main(String[] args) {
        OptVarHandleSem v = new OptVarHandleSem();
        v.ref = "x";
        Object[] arr = new Object[8];
        arr[3] = v;
        int[] ints = new int[4];
        long s = 0;
        for (int r = 0; r < 50; r++) s += run(v, arr, ints, 20000);
        System.out.println(s + " " + v.ref + " " + v.n + " " + v.big + " " + java.util.Arrays.toString(ints));
        try { AA.getVolatile(arr, 8); } catch (IndexOutOfBoundsException e) { System.out.println(e.getMessage()); }
        try { AA.setVolatile(new String[1], 0, 1); } catch (ClassCastException | ArrayStoreException e) { System.out.println(e.getClass().getSimpleName()); }
        try { REF.getVolatile((Object) null); } catch (NullPointerException e) { System.out.println("npe"); }
    }
}
