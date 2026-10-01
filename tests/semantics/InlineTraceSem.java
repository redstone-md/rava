import java.util.*;
import java.util.stream.*;
// Stack traces and stack walks taken from inside small nested methods that compiled code inlines.
public class InlineTraceSem {
    static int depth;
    static String names(StackTraceElement[] st) {
        return Arrays.stream(st).limit(5).map(e -> e.getMethodName() + ":" + e.getLineNumber()).collect(Collectors.joining(","));
    }
    static String trace() { return names(new Throwable().getStackTrace()); }
    static String leaf() { return trace(); }
    static String mid() { return leaf(); }
    static int boom(int[] a, int i) { return a[i]; }
    static int viaBoom(int[] a, int i) { return boom(a, i) + 1; }
    static String walk() { return StackWalker.getInstance().walk(s -> s.limit(4).map(StackWalker.StackFrame::getMethodName).collect(Collectors.joining(","))); }
    static String walkMid() { return walk(); }
    String inst() { return mid(); }
    static Class<?> caller() { return StackWalker.getInstance(StackWalker.Option.RETAIN_CLASS_REFERENCE).getCallerClass(); }
    static Class<?> callerMid() { return caller(); }

    public static void main(String[] args) {
        Set<String> seen = new TreeSet<>();
        int[] arr = new int[2];
        for (int i = 0; i < 20000; i++) {
            seen.add("mid " + mid());
            seen.add("inst " + new InlineTraceSem().inst());
            seen.add("walk " + walkMid());
            seen.add("caller " + callerMid().getSimpleName());
            try {
                viaBoom(arr, i % 3);
            } catch (ArrayIndexOutOfBoundsException e) {
                if (e.getStackTrace().length > 0) seen.add("aioobe " + names(e.getStackTrace()));
            }
        }
        seen.forEach(System.out::println);
    }
}
