import java.util.*;

/** The shape Mixin gives a modded hot method: a cancellable @Inject handler that gets a fresh
 *  CallbackInfoReturnable on every call, then the original body (a HashMap lookup keyed by an
 *  object with identity hash, as LivingEntity.hasEffect(Holder)). */
public class MixinBench {
    static final class CallbackInfoReturnable<R> {
        final String name; final boolean cancellable; R value; boolean cancelled;
        CallbackInfoReturnable(String name, boolean cancellable) { this.name = name; this.cancellable = cancellable; }
        boolean isCancelled() { return cancelled; }
        R getReturnValue() { return value; }
        void setReturnValue(R r) { value = r; cancelled = true; }
    }
    static final class Holder { final int id; Holder(int id) { this.id = id; } }
    static abstract class Living {
        final Map<Holder, Object> effects = new HashMap<>();
        final List<Object> armor = Arrays.asList(new Object[4]);
        abstract Object getItemBySlot(int slot);
        boolean hasEffect(Holder h) {
            CallbackInfoReturnable<Boolean> ci = new CallbackInfoReturnable<>("hasEffect", true);
            handler(h, ci);
            if (ci.isCancelled()) return ci.getReturnValue();
            return effects.containsKey(h);
        }
        void handler(Holder h, CallbackInfoReturnable<Boolean> ci) {
            for (int s = 0; s < 4; s++) if (getItemBySlot(s) == MARKER) { ci.setReturnValue(false); return; }
        }
    }
    static final Object MARKER = new Object();
    static class Mob extends Living { Object getItemBySlot(int s) { return armor.get(s); } }
    static final class Cow extends Mob {}
    static final class Sheep extends Mob {}
    static final class Pig extends Mob {}
    static final class Villager extends Mob {}

    public static void main(String[] args) {
        Holder[] hs = new Holder[20];
        for (int i = 0; i < hs.length; i++) hs[i] = new Holder(i);
        List<Living> ls = new ArrayList<>();
        for (int i = 0; i < 400; i++) {
            Living l = switch (i % 4) { case 0 -> new Cow(); case 1 -> new Sheep(); case 2 -> new Pig(); default -> new Villager(); };
            l.effects.put(hs[i % 20], "x");
            ls.add(l);
        }
        Living[] arr = ls.toArray(new Living[0]);
        long check = 0;
        for (int round = 0; round < 12; round++) {
            long t0 = System.nanoTime();
            check += round(arr, hs);
            System.out.printf("%.2f ns/call%n", (System.nanoTime() - t0) / (500.0 * arr.length * 5));
        }
        System.out.println(check);
    }

    /** One timed round, in a method of its own: `main` deoptimizing at its printf does not
     *  throw the round back into slower code. */
    static long round(Living[] arr, Holder[] hs) {
        long check = 0;
        for (int rep = 0; rep < 500; rep++)
            for (Living a : arr) for (int k = 0; k < 5; k++) if (a.hasEffect(hs[(rep + k) % 20])) check++;
        return check;
    }
}
