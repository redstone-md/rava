import java.lang.invoke.MethodHandles;
import java.lang.reflect.Array;
import java.util.Arrays;

// Array.newInstance and Arrays.copyOf with a component class known at compile time allocate
// inline; getComponentType of a known class folds. Other components (primitive, void, unknown),
// negative lengths and method handles resolved again and again keep their behavior.
public class ReflectArraySem {
    static final Class<?>[] TYPES = { int.class, String.class, Object.class };
    static int sink;
    public void target(Object o) { sink += o.hashCode() & 3; }

    static int copies(int n) {
        Class<?>[] a = Arrays.copyOf(TYPES, (n & 3) + 1);
        String[] s = Arrays.copyOf(new String[] { "a", "b" }, n & 7);
        Object[] o = (Object[]) Array.newInstance(Runnable.class, n & 5);
        int[][] grid = (int[][]) Array.newInstance(int[].class, 2);
        return a.length + s.length * 3 + o.length * 5 + grid.length + a.getClass().getComponentType().getName().length()
            + o.getClass().getComponentType().getName().length() + (s.length > 0 && s[0] != null ? 1 : 0);
    }

    public static void main(String[] args) throws Throwable {
        long acc = 0;
        var m = ReflectArraySem.class.getMethod("target", Object.class);
        var self = new ReflectArraySem();
        for (int r = 0; r < 30000; r++) {
            acc += copies(r);
            MethodHandles.lookup().unreflect(m).bindTo(self).invokeWithArguments("x" + (r & 7));
        }
        System.out.println("acc " + acc + " " + sink);
        try { Array.newInstance(String.class, -1); } catch (NegativeArraySizeException e) { System.out.println("nase " + e.getMessage()); }
        try { Array.newInstance(void.class, 1); } catch (IllegalArgumentException e) { System.out.println("iae"); }
        System.out.println(Array.newInstance(int.class, 3).getClass() + " " + int[].class.getComponentType() + " " + String.class.getComponentType());
    }
}
