// Reference.refersTo, which optimized code reads as the referent compared with its argument:
// before and after clear(), with null, with another object, through ThreadLocal lookups (each a
// refersTo of its entry's key), and with references cleared between the calls of a hot loop.
import java.lang.ref.PhantomReference;
import java.lang.ref.ReferenceQueue;
import java.lang.ref.WeakReference;

public class RefersToSem {
    static int check(WeakReference<Object> w, Object o, Object other) {
        int r = 0;
        if (w.refersTo(o)) r |= 1;
        if (w.refersTo(other)) r |= 2;
        if (w.refersTo(null)) r |= 4;
        return r;
    }

    public static void main(String[] args) {
        Object o = new Object(), other = new Object();
        PhantomReference<Object> p = new PhantomReference<>(o, new ReferenceQueue<>());
        long sum = 0;
        for (int i = 0; i < 300_000; i++) {
            WeakReference<Object> w = new WeakReference<>(o);
            sum += check(w, o, other);
            if ((i & 7) == 0) w.clear();
            sum += check(w, o, other) * 3;
            sum += p.refersTo(o) ? 5 : 0;
        }
        System.out.println(sum);
        ThreadLocal<String> a = ThreadLocal.withInitial(() -> "a"), b = new ThreadLocal<>();
        StringBuilder out = new StringBuilder();
        for (int i = 0; i < 200_000; i++) {
            if (i == 100) b.set("b");
            if (i == 150_000) b.remove();
            if (i % 50_000 == 0) out.append(a.get()).append(b.get()).append(' ');
        }
        System.out.println(out);
    }
}
