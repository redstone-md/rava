import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.function.BiConsumer;
import java.util.function.Consumer;
import java.util.function.Supplier;

/** A synthetic per-tick event-dispatch workload: 40 systems register listeners from two lambda
 *  sites and five system implementation classes. Each listener looks up attachment data, checks
 *  an interface flag, pushes section names to either an inactive or recording profiler, and runs
 *  a system callback. Reports ns per tick after warm-up. */
public class SystemTickBench {
    interface Profiler { void push(String s); void pop(); long checksum(); }
    static final class InactiveProfiler implements Profiler {
        static final InactiveProfiler INSTANCE = new InactiveProfiler();
        public void push(String s) {}
        public void pop() {}
        public long checksum() { return 0; }
    }
    static final class RecordingProfiler implements Profiler {
        long checksum;
        public void push(String s) { checksum = checksum * 31 + s.hashCode(); }
        public void pop() {}
        public long checksum() { return checksum; }
    }
    static final class Key {
        final String namespace, path;
        Key(String namespace, String path) { this.namespace = namespace; this.path = path; }
        String toLanguageKey() { return namespace + "." + path; }
    }
    static final class AttachmentType { final int id; AttachmentType(int id) { this.id = id; } }
    /** A registry entry bound on first use, as a deferred holder is. */
    static final class Holder<T> implements Supplier<T> {
        private final Supplier<T> factory;
        private T value;
        Holder(Supplier<T> factory) { this.factory = factory; }
        public T get() {
            T v = value;
            if (v == null) value = v = factory.get();
            return v;
        }
    }
    static class Level {
        final Key dimension = new Key("minecraft", "overworld");
        final Profiler profiler;
        private Map<AttachmentType, Object> attachments;
        final boolean clientSide = false;
        long gameTime;
        Level(Profiler profiler) { this.profiler = profiler; }
        Profiler getProfiler() { return profiler; }
        boolean isClientSide() { return clientSide; }
        @SuppressWarnings("unchecked")
        <T> T getData(Supplier<AttachmentType> type, Supplier<T> init) {
            if (attachments == null) attachments = new IdentityHashMap<>(4);
            AttachmentType t = type.get();
            Object v = attachments.get(t);
            if (v == null) attachments.put(t, v = init.get());
            return (T) v;
        }
    }
    static final class TickEvent {
        final Level level;
        TickEvent(Level level) { this.level = level; }
        Level getLevel() { return level; }
    }
    interface SystemData { boolean enabled(); }

    static abstract class LevelSystem<T extends SystemData> {
        LevelSystem(List<Consumer<TickEvent>> bus) {
            bus.add(e -> tick(e, "system.pre", this::preTick));
            bus.add(e -> tick(e, "system.post", this::postTick));
        }
        abstract Supplier<AttachmentType> attachmentType();
        abstract T newData();
        boolean overridesEnabledFlag() { return false; }
        void preTick(TickEvent e, T data) {}
        void postTick(TickEvent e, T data) {}
        private void tick(TickEvent e, String name, BiConsumer<TickEvent, T> body) {
            Level level = e.getLevel();
            if (level.isClientSide()) return;
            T data = level.getData(attachmentType(), this::newData);
            if (!overridesEnabledFlag() && !data.enabled()) return;
            Profiler p = level.getProfiler();
            p.push(name);
            p.push(level.dimension.toLanguageKey());
            body.accept(e, data);
            p.pop();
            p.pop();
        }
    }

    static final int SYSTEMS = 40;
    static final List<Consumer<TickEvent>> BUS = new ArrayList<>();
    static final List<LevelSystem<?>> SYSTEM_LIST = new ArrayList<>();

    /** One system class per index: a data class with a counter and a flag. */
    static abstract class Data implements SystemData {
        int ticks;
        boolean on = true;
        public boolean enabled() { return on; }
    }
    static final class D0 extends Data {} static final class D1 extends Data {} static final class D2 extends Data {}
    static final class D3 extends Data {} static final class D4 extends Data {} static final class D5 extends Data {}
    static final class D6 extends Data {} static final class D7 extends Data {}

    static <D extends Data> void register(Supplier<D> make, int i, boolean work) {
        Holder<AttachmentType> type = new Holder<>(() -> new AttachmentType(i));
        LevelSystem<D> s = switch (i % 5) {
            case 0 -> new LevelSystem<D>(BUS) {
                Supplier<AttachmentType> attachmentType() { return type; }
                D newData() { return make.get(); }
                void postTick(TickEvent e, D d) { d.ticks++; }
            };
            case 1 -> new LevelSystem<D>(BUS) {
                Supplier<AttachmentType> attachmentType() { return type; }
                D newData() { return make.get(); }
                void preTick(TickEvent e, D d) { if (work) d.ticks += (int) e.getLevel().gameTime & 1; }
            };
            case 2 -> new LevelSystem<D>(BUS) {
                Supplier<AttachmentType> attachmentType() { return type; }
                D newData() { return make.get(); }
                boolean overridesEnabledFlag() { return true; }
                void postTick(TickEvent e, D d) { d.ticks ^= 1; }
            };
            case 3 -> new LevelSystem<D>(BUS) {
                Supplier<AttachmentType> attachmentType() { return type; }
                D newData() { return make.get(); }
            };
            default -> new LevelSystem<D>(BUS) {
                Supplier<AttachmentType> attachmentType() { return type; }
                D newData() { return make.get(); }
                void preTick(TickEvent e, D d) { d.ticks--; }
                void postTick(TickEvent e, D d) { d.ticks++; }
            };
        };
        SYSTEM_LIST.add(s);
    }

    /** One server tick: every listener once (compiled on its own, as a real tick is, not
     *  entered in the middle of a loop). */
    static void tick(Level level, TickEvent event) {
        level.gameTime++;
        for (Consumer<TickEvent> l : BUS) l.accept(event);
    }

    public static void main(String[] args) {
        List<Supplier<Data>> makers = List.of(D0::new, D1::new, D2::new, D3::new, D4::new, D5::new, D6::new, D7::new);
        for (int i = 0; i < SYSTEMS; i++) register(makers.get(i % makers.size()), i, i % 3 == 0);
        Profiler profiler = args.length > 2 && args[2].equals("active")
                ? new RecordingProfiler() : InactiveProfiler.INSTANCE;
        Level level = new Level(profiler);
        TickEvent event = new TickEvent(level);
        int ticks = args.length > 1 ? Integer.parseInt(args[1]) : 20_000;
        int rounds = args.length > 0 ? Integer.parseInt(args[0]) : 15;
        double[] samples = new double[rounds];
        for (int round = 0; round < rounds; round++) {
            samples[round] = round(level, event, ticks);
        }
        long sum = 0;
        for (LevelSystem<?> s : SYSTEM_LIST) sum += ((Data) level.getData(s.attachmentType(), () -> null)).ticks;
        for (double sample : samples) System.out.printf(Locale.ROOT, "tick %.1f ns%n", sample);
        System.out.println("check " + sum);
        System.out.println("profile " + profiler.checksum());
    }

    /** Keep output and the round loop in separate methods so printing cannot deoptimize work
     *  before the next sample. */
    static double round(Level level, TickEvent event, int ticks) {
        long t0 = System.nanoTime();
        for (int t = 0; t < ticks; t++) tick(level, event);
        return (System.nanoTime() - t0) / (double) ticks;
    }
}
