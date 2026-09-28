// Differential probe: CompletableFuture, atomics, concurrent collections and ByteBuffer.
// Deterministic on purpose — no sleeps, no racing, no thread-count assumptions.
import java.nio.*;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
public class ConcSem {
    static void p(String k, Object v) { System.out.println(k + " " + v); }
    static String ex(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (Throwable t) {
            Throwable c = t.getCause();
            return t.getClass().getName() + (c == null ? "" : " cause=" + c.getClass().getName() + ":" + c.getMessage());
        }
    }
    public static void main(String[] a) throws Exception {
        // --- CompletableFuture: composition and completion
        CompletableFuture<Integer> f = CompletableFuture.completedFuture(21);
        p("cf.get", f.get() + " " + f.join() + " " + f.isDone() + " " + f.isCompletedExceptionally());
        p("cf.thenApply", f.thenApply(x -> x * 2).join());
        p("cf.thenCompose", f.thenCompose(x -> CompletableFuture.completedFuture(x + 1)).join());
        p("cf.thenCombine", f.thenCombine(CompletableFuture.completedFuture(1), Integer::sum).join());
        p("cf.thenAccept", accept());
        CompletableFuture<Integer> boom = new CompletableFuture<>();
        boom.completeExceptionally(new IllegalStateException("nope"));
        p("cf.failed", boom.isCompletedExceptionally() + " " + ex(boom::join));
        p("cf.exceptionally", boom.exceptionally(t -> -1).join());
        p("cf.handle", boom.handle((v, t) -> t == null ? "v" + v : "t" + t.getClass().getSimpleName()).join());
        p("cf.allOf", CompletableFuture.allOf(f, CompletableFuture.completedFuture(2)).isDone());
        p("cf.anyOf", CompletableFuture.anyOf(f).join());
        CompletableFuture<String> late = new CompletableFuture<>();
        p("cf.complete", late.complete("x") + " " + late.complete("y") + " " + late.join());
        p("cf.getNow", new CompletableFuture<String>().getNow("d"));
        p("cf.supply", CompletableFuture.supplyAsync(() -> 5).join());
        p("cf.runAsync", CompletableFuture.runAsync(() -> {}).join());

        // --- atomics
        AtomicInteger ai = new AtomicInteger(5);
        p("ai", ai.get() + " " + ai.incrementAndGet() + " " + ai.getAndIncrement() + " " + ai.get());
        p("ai.cas", ai.compareAndSet(7, 10) + " " + ai.compareAndSet(99, 0) + " " + ai.get());
        p("ai.update", ai.updateAndGet(x -> x * 2) + " " + ai.accumulateAndGet(3, Integer::sum));
        p("ai.addAndGet", ai.addAndGet(-3) + " " + ai.getAndSet(1) + " " + ai.get());
        AtomicLong al = new AtomicLong(1L << 40);
        p("al", al.get() + " " + al.incrementAndGet());
        AtomicReference<String> ar = new AtomicReference<>("a");
        p("ar", ar.get() + " " + ar.compareAndSet("a", "b") + " " + ar.get() + " " + ar.getAndSet("c"));
        AtomicBoolean ab = new AtomicBoolean();
        p("ab", ab.get() + " " + ab.compareAndSet(false, true) + " " + ab.get());

        // --- concurrent collections
        ConcurrentHashMap<String, Integer> chm = new ConcurrentHashMap<>();
        chm.put("a", 1);
        p("chm", chm.putIfAbsent("a", 9) + " " + chm.putIfAbsent("b", 2) + " " + new TreeMap<>(chm));
        p("chm.compute", chm.compute("a", (k, v) -> v + 10) + " " + chm.computeIfAbsent("c", k -> 3));
        p("chm.merge", chm.merge("a", 5, Integer::sum) + " " + chm.remove("c"));
        p("chm.nullKey", ex(() -> chm.put(null, 1)));
        p("chm.nullVal", ex(() -> chm.put("z", null)));
        CopyOnWriteArrayList<String> cow = new CopyOnWriteArrayList<>(List.of("x", "y"));
        cow.add("z");
        p("cow", cow + " " + cow.size());
        ConcurrentLinkedQueue<Integer> clq = new ConcurrentLinkedQueue<>(List.of(1, 2));
        p("clq", clq.poll() + " " + clq.peek() + " " + clq.size());

        // --- ByteBuffer: the packet path
        ByteBuffer bb = ByteBuffer.allocate(16);
        bb.putInt(0x01020304).putShort((short) 5).put((byte) 6);
        p("bb.pos", bb.position() + " " + bb.limit() + " " + bb.capacity() + " " + bb.remaining());
        bb.flip();
        p("bb.read", Integer.toHexString(bb.getInt()) + " " + bb.getShort() + " " + bb.get() + " " + bb.remaining());
        bb.rewind();
        p("bb.rewind", bb.position() + " " + Integer.toHexString(bb.getInt()));
        ByteBuffer le = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN);
        le.putInt(0x01020304).flip();
        p("bb.order", Integer.toHexString(le.getInt()) + " " + le.order());
        ByteBuffer wrap = ByteBuffer.wrap(new byte[]{ 10, 20, 30 });
        p("bb.wrap", wrap.get() + " " + wrap.get() + " " + wrap.remaining() + " " + wrap.hasRemaining());
        p("bb.oob", ex(() -> ByteBuffer.allocate(2).getInt()));
        byte[] dst = new byte[2];
        ByteBuffer.wrap(new byte[]{ 1, 2, 3 }).get(dst);
        p("bb.bulk", Arrays.toString(dst));
        p("bb.compact", compact());
        p("bb.slice", slice());
    }
    static String accept() {
        StringBuilder b = new StringBuilder();
        CompletableFuture.completedFuture(3).thenAccept(v -> b.append("got").append(v)).join();
        return b.toString();
    }
    static String compact() {
        ByteBuffer b = ByteBuffer.wrap(new byte[]{ 1, 2, 3, 4 });
        b.get(); b.get();
        b.compact();
        return b.position() + "/" + b.limit();
    }
    static String slice() {
        ByteBuffer b = ByteBuffer.allocate(8);
        b.position(2);
        ByteBuffer s = b.slice();
        return s.capacity() + "/" + s.position();
    }
}
