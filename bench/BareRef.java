// PRICES THE VALUE-MODEL ARC'S FINAL FLIP, on a workload where that flip is SOUND.
//
// The arc (docs/value-model-rewrite-plan.md) pays only at the end: once no non-0b00 ref cell can
// persist, the JIT emits a bare `load` for every ref field instead of a tag-classify diamond. The
// ring's hottest method carries ~1454 band/icmp/select/cmov/sete/testb instructions for four lines
// of Java, and the whole case for a multi-year arc is that those disappear. That has been asserted
// repeatedly -- including by me -- and never measured.
//
// It cannot be measured on the real workload: RAVA_JIT_BARE_REF_LOAD is UNSOUND while any handle
// (0b01) or boxed (0b11) cell exists, so a boot with it on dies before the density functions even
// compile. And a class that survives (RingScan) turned out to compile byte-identically either way,
// because its hot body is numeric and never touches the arena ref-field emitters the flag gates.
//
// THIS CLASS IS BUILT TO SATISFY BOTH CONSTRAINTS AT ONCE:
//   * every object it touches is its OWN plain class, allocated here, with a complete field layout
//     and no native-backed super -- so all of them are arena-born 0b00 and the flag is SOUND for
//     this program, which means it can be RUN and not merely dumped;
//   * the hot loop is nothing but REFERENCE FIELD reads and writes (`next`, `payload`), which is
//     precisely what emit_arena_getfield_ref_flat / the store side gate -- so the flag actually
//     changes its code, which RingScan's did not.
//
// The result is a ceiling in two forms: an instruction-count diff (load-independent, measurable on
// a busy machine) and, on a quiet one, a wall number. Neither is the arc's value on the REAL boot
// -- this is a best case, since every ref here is already 0b00 -- but a best case is exactly what a
// ceiling is, and it is the difference between a priced plan and a hopeful one.
public class BareRef {
    static final class Node {
        Node next;       // ref field: the getfield the flip collapses
        Payload payload; // second ref field, so the loop is not a single repeated slot
        long tag;
        Node(long tag) { this.tag = tag; }
    }
    static final class Payload {
        long a, b;
        Payload(long a, long b) { this.a = a; this.b = b; }
    }

    /// Walks a ring of nodes, reading TWO ref fields per step and writing one back, so the loop is
    /// dominated by exactly the arena ref-field getfield/putfield the flag gates. The arithmetic
    /// exists only to make the result observable and unfoldable.
    static long walk(Node head, int steps) {
        long acc = 0;
        Node cur = head;
        for (int i = 0; i < steps; i++) {
            Payload p = cur.payload;      // arena getfield, ref
            acc += p.a ^ (p.b + cur.tag);
            Node nxt = cur.next;          // arena getfield, ref
            nxt.payload = p;              // arena putfield, ref
            cur = nxt;
        }
        return acc;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 4096;
        int steps = args.length > 1 ? Integer.parseInt(args[1]) : 20_000_000;
        Node[] ring = new Node[n];
        for (int i = 0; i < n; i++) {
            ring[i] = new Node(i * 2654435761L);
            ring[i].payload = new Payload(i, i * 31L);
        }
        for (int i = 0; i < n; i++) ring[i].next = ring[(i + 1) % n];

        for (int w = 0; w < 3; w++) walk(ring[0], steps / 10);   // warm the JIT
        long t0 = System.nanoTime();
        long s = walk(ring[0], steps);
        long ns = System.nanoTime() - t0;
        System.out.println("BAREREF_HASH=" + Long.toHexString(s));
        System.out.println("BAREREF_MS=" + (ns / 1_000_000));
    }
}
