// Caller-sensitive methods called from hot, inlined code: MethodHandles.lookup() and
// Class.forName report the class of the method that called them, whether the caller is a
// helper of another class, a lambda, a method reached through reflection (whose
// Method.invoke frames do not count) or code inlined into a caller of a different class.
import java.lang.invoke.MethodHandles;
import java.lang.reflect.Method;
import java.util.function.Supplier;

public class CallerClassSem {
    static final class Helper {
        static Class<?> look() { return MethodHandles.lookup().lookupClass(); }
        static Class<?> viaHelper() { return look(); }
        Class<?> instance() { return MethodHandles.lookup().lookupClass(); }
        static Class<?> forName() throws Exception { return Class.forName("CallerClassSem$Other"); }
    }
    static final class Other {
        static Class<?> look() { return MethodHandles.lookup().lookupClass(); }
        public static Class<?> reflected() { return MethodHandles.lookup().lookupClass(); }
    }

    static Class<?> mine() { return MethodHandles.lookup().lookupClass(); }

    public static void main(String[] args) throws Exception {
        Supplier<Class<?>> lambda = () -> MethodHandles.lookup().lookupClass();
        Method reflected = Other.class.getMethod("reflected");
        Helper h = new Helper();
        int[] counts = new int[8];
        for (int i = 0; i < 60000; i++) {
            if (mine() == CallerClassSem.class) counts[0]++;
            if (Helper.look() == Helper.class) counts[1]++;
            if (Helper.viaHelper() == Helper.class) counts[2]++;
            if (h.instance() == Helper.class) counts[3]++;
            if (Other.look() == Other.class) counts[4]++;
            if (lambda.get() == CallerClassSem.class) counts[5]++;
            if (reflected.invoke(null) == Other.class) counts[6]++;
            if (Helper.forName() == Other.class) counts[7]++;
        }
        System.out.println(java.util.Arrays.toString(counts));
    }
}
