import java.util.*;
public class CiMini {
    public static void main(String[] a) {
        System.out.println("before");
        Comparator<String> c = String.CASE_INSENSITIVE_ORDER;
        System.out.println("got " + (c != null));
        String[] ci = { "b", "A" };
        Arrays.sort(ci, c);
        System.out.println("sorted " + Arrays.toString(ci));
    }
}
