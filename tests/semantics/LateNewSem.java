// Late allocation must preserve initialization order, inherited fields and live roots.
public class LateNewSem {
    static int phase;
    static int initialized;

    static class Base {
        long inherited = 19;
    }

    static class Item extends Base {
        static Item duringInit;
        static {
            phase = 1;
            duringInit = make(7);
            phase = 2;
            initialized++;
        }
        final int value;
        final int bornPhase;
        Item(int value) {
            this.value = value;
            this.bornPhase = phase;
        }
    }

    static Item make(int value) { return new Item(value); }
    static long consume(Item kept, Item fresh) {
        return kept.value + kept.inherited + fresh.value + fresh.inherited;
    }

    public static void main(String[] args) {
        Item kept = make(73);
        System.out.println("init=" + initialized + ":" + Item.duringInit.bornPhase
                + ":" + kept.bornPhase + ":" + Item.duringInit.value);
        long sum = 0;
        for (int i = 0; i < 40000; i++) {
            // kept stays live while the second argument allocates and may collect.
            sum += consume(kept, make(i));
        }
        System.out.println("sum=" + sum);
        System.out.println("kept=" + kept.value + ":" + kept.inherited + ":" + phase);
    }
}
