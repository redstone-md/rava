import java.util.*;

/** Reference copies that need an element check (an Object[] into a String[] or an interface
 *  array), hot in compiled code: whole copies, and a failing element that leaves the elements
 *  before it copied; stores of one- and two-element lists into fresh arrays. */
public class CheckedCopySem {
    interface Shape { int sides(); }
    record Tri() implements Shape { public int sides() { return 3; } }
    record Quad() implements Shape { public int sides() { return 4; } }

    static int round(Object[] src, List<Object> list, int i) {
        String[] a = list.toArray(new String[0]);
        String[] b = Arrays.copyOf(src, 3, String[].class);
        CharSequence[] c = Arrays.copyOfRange(src, 1, 4, CharSequence[].class);
        String[] d = new String[5];
        System.arraycopy(src, 0, d, 1, 4);
        List<Shape> one = List.of(new Tri()), two = List.of(new Tri(), new Quad());
        Shape[] s1 = one.toArray(new Shape[0]), s2 = two.toArray(new Shape[(i & 1) * 2]);
        return a.length + b[2].length() + c[2].length() + d[4].length() + s1[0].sides() + s2[1].sides() + s1.length + s2.length;
    }

    public static void main(String[] args) {
        List<Object> list = new ArrayList<>();
        for (int i = 0; i < 6; i++) list.add("e" + i);
        Object[] src = list.toArray();
        long sum = 0;
        for (int i = 0; i < 200_000; i++) sum += round(src, list, i);
        System.out.println("sum " + sum);
        for (int k = 0; k < 3; k++) {
            Object[] mixed = {"x", "y", k, "z"};
            String[] dst = new String[4];
            try {
                System.arraycopy(mixed, 0, dst, 0, 4);
                System.out.println("no exception");
            } catch (ArrayStoreException e) {
                System.out.println("ASE " + Arrays.toString(dst));
            }
            try {
                Arrays.copyOf(mixed, 4, String[].class);
            } catch (ArrayStoreException e) {
                System.out.println("ASE copyOf");
            }
            Object[] nulls = {null, "a", null};
            System.out.println(Arrays.toString(Arrays.copyOf(nulls, 3, String[].class)));
        }
    }
}
