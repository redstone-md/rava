// sem-flags: --enable-preview --release 21
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.ValueLayout;

// Off-heap memory from the Foreign Memory API (preview in Java 21): a shared arena is
// closed through a VM native that checks no thread is mid-access. After closing, the memory is
// gone and every access fails.
public class SharedArenaSem {
    public static void main(String[] args) throws Exception {
        Arena arena = Arena.ofShared();
        MemorySegment seg = arena.allocate(64);
        Thread writer = new Thread(() -> {
            for (int i = 0; i < 16; i++) seg.setAtIndex(ValueLayout.JAVA_INT, i, i * i);
        });
        writer.start();
        writer.join();
        long sum = 0;
        for (int i = 0; i < 16; i++) sum += seg.getAtIndex(ValueLayout.JAVA_INT, i);
        System.out.println("sum " + sum);
        arena.close();
        System.out.println("alive " + seg.scope().isAlive());
        try {
            seg.get(ValueLayout.JAVA_INT, 0);
        } catch (IllegalStateException e) {
            System.out.println("after close: " + e.getClass().getSimpleName());
        }
        try (Arena confined = Arena.ofConfined()) {
            System.out.println("confined " + confined.allocate(8).byteSize());
        }
    }
}
