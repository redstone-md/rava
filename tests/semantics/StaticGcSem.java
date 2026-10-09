// Young objects stored only in static fields survive young collections whichever way they
// were stored: putstatic from interpreted and compiled code, reflection, a VarHandle, and a
// static written back and forth between collections; statics of classes loaded late too.
import java.lang.invoke.MethodHandles;
import java.lang.invoke.VarHandle;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.List;

public class StaticGcSem {
    static final class Box { final int v; final int[] pad = new int[6]; Box(int v) { this.v = v; } }
    static Box a, b, c;
    static Object[] arr;
    static List<Box> list;
    static final class Late { static Box x; static Box y; }
    static Box viaReflect, viaHandle;
    static Object sink;

    static int churn(int n) { int s = 0; for (int i = 0; i < n; i++) { Object[] o = new Object[8]; o[0] = new Box(i); sink = o; s += o.length; } return s; }

    public static void main(String[] args) throws Throwable {
        Field rf = StaticGcSem.class.getDeclaredField("viaReflect");
        VarHandle vh = MethodHandles.lookup().findStaticVarHandle(StaticGcSem.class, "viaHandle", Box.class);
        long check = 0;
        for (int r = 0; r < 300; r++) {
            a = new Box(r); b = new Box(r * 2); c = (r % 3 == 0) ? null : new Box(r * 3);
            arr = new Object[] { new Box(r + 1), "s" + r, new int[] { r } };
            list = new ArrayList<>(); for (int i = 0; i < 20; i++) list.add(new Box(r + i));
            rf.set(null, new Box(r + 7));
            vh.set(new Box(r + 9));
            if (r == 150) { Late.x = new Box(-1); }
            if (r > 150) { Late.y = new Box(r - 150); }
            Box keep = a;
            churn(20000);
            a = new Box(-5); churn(5000); a = keep;   // changed and changed back across collections
            churn(20000);
            check += a.v + b.v + (c == null ? 0 : c.v) + ((Box) arr[0]).v + ((String) arr[1]).length() + ((int[]) arr[2])[0];
            for (Box x : list) check += x.v;
            check += ((Box) rf.get(null)).v + ((Box) vh.get()).v;
            if (Late.x != null) check += Late.x.v;
            if (Late.y != null) check += Late.y.v;
        }
        System.out.println("check " + check);
    }
}
