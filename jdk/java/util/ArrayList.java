package java.util;

import java.util.function.Consumer;
import java.util.function.Predicate;
import java.util.function.UnaryOperator;

/**
 * `java.util.ArrayList` AS GUEST BYTECODE.
 *
 * <p>Why this exists, measured rather than assumed. `bench/GuestColl.java` runs an array-backed
 * list twice in one process — once as our Rust native and once as ordinary guest bytecode
 * compiled by our own JIT — with the checksums asserted equal:
 *
 * <pre>
 *   rava   list native 227 ns/element   list guest 45   = 5.0x
 *   java   list native   1 ns/element   list guest  1
 * </pre>
 *
 * <p>On HotSpot the two are identical, so there is no algorithmic advantage to normalise away:
 * the 5x is purely what our engine does to the same work depending on which side of the native
 * boundary it runs on. The handle census says where that lands — 6.23M handle births per boot,
 * 96% of them collection wrappers — and those drive the heap/intern, allocator, GC and `Value`
 * categories that are ~24% of a boot. A list written here allocates a plain `Object[]` in the
 * arena and creates no handle at all.
 *
 * <h2>The backing field is `$elems`, and that is not a style choice</h2>
 *
 * <p>Our natives FABRICATE `java/util/ArrayList` instances themselves — `Arrays.asList`,
 * `List.of`, the stream collectors, `Files.list`, the map views, 15 construction sites in all —
 * and they populate them by writing the synthetic field `$elems`, whose array LENGTH is the
 * size. There is no separate size field to write.
 *
 * <p>So this class uses the same convention rather than a private `a`/`n` pair: a guest list and
 * a native-fabricated one are then the same object shape, and neither side needs to know which
 * built it. The first version used `a`/`n` and MC died in 0.6 s with `IllegalArgumentException:
 * need at least one option`, which one probe explained: `Arrays.asList("a","b","c").size() == 0`
 * — the guest `size()` was reading a field the native had never written.
 *
 * <p>The price of matching the native convention is that `add` reallocates, because a Java array
 * cannot grow. The native gets amortised growth for free by backing `$elems` with a Rust `Vec`;
 * we cannot see that capacity from here. Appends are therefore O(n) for now — correct first,
 * and cheap to fix later by giving the shared convention an explicit size field once the
 * producers migrate too.
 */
public class ArrayList<E> implements List<E>, RandomAccess, Cloneable, java.io.Serializable {
    /**
     * Shared with the natives. May hold SPARE CAPACITY beyond `$size` — see the class comment.
     */
    Object[] $elems;

    /**
     * The logical length. Natives read it through `Heap::elems_len`, which falls back to the
     * array length when the field is absent — which is every collection the natives fabricate.
     */
    int $size;

    /**
     * The natives' immutability contract, read straight out of the field they stamp.
     *
     * <p>⛔A guest mutator that ignores this is not a slower `ArrayList`, it is a WRONG one:
     * `List.of("a","b").add("c")` silently succeeded and the list read `[a, b, c]`, so
     * `of.equals(List.of("a","b"))` answered false and MC refused its own data pack — "Found new
     * data pack vanilla, but can't load it due to missing features minecraft:vanilla".
     *
     * <p>The kinds are `immutable.rs`: 0 none, 1 immutable (every mutator throws), 2
     * `Arrays.asList` (fixed SIZE, `set`/`sort`/`replaceAll` write through), 3
     * `Collections.empty*`/`nCopies` (fixed size AND no assignment), 4 the singletons (as 3, and
     * `removeIf` throws unconditionally), 5 a map view (removal fine, addition throws).
     */
    int $immutable;

    /**
     * A DELEGATING source — set by `Collections.unmodifiable*` when it wraps an instance of this
     * class, and by nothing else.
     *
     * <p>The natives build such a wrapper by sharing the source's `$elems` array, which is live
     * only until the source grows and replaces it. Following the source object instead is live
     * by construction, and it is what makes `unmodifiableList(x)` a distinct object rather than
     * `x` itself — the arena path could not build a wrapper at all before, so the "view" was the
     * argument and `un.add(...)` simply succeeded.
     */
    Object $usrc;

    /**
     * Structural modification count, for the fail-fast iterators.
     *
     * <p>⛔Without it `for (String s : list) list.add(x)` does not throw — it LOOPS, because
     * `hasNext` keeps seeing a longer list. Measured: the probe grew the list until the process
     * died on a 1 GiB allocation. A missing `ConcurrentModificationException` is not a cosmetic
     * divergence here, it is a hang.
     */
    transient int $mod;

    /**
     * Has a constructor of THIS class run on this object?
     *
     * <p>⛔It has to be an explicit flag, and the first version tried to infer it instead: "`$size`
     * is 0 but `$elems` is not empty" was read as "a native built this". That inference is sound
     * only while the array length IS the size — the moment the class keeps spare capacity,
     * `new ArrayList<>(10)` looks exactly like a native-built list of ten elements, and MC died
     * in 0.9 s. Natives never write this field, so `false` means native-fabricated, always.
     */
    boolean $init;

    public ArrayList() {
        $elems = new Object[10];
        $size = 0;
        $init = true;
    }

    public ArrayList(int cap) {
        $elems = new Object[cap < 10 ? 10 : cap];
        $size = 0;
        $init = true;
    }

    public ArrayList(Collection<? extends E> c) {
        $elems = c.toArray();
        $size = $elems.length;
        $init = true;
    }

    /**
     * A native-fabricated instance never ran a constructor, so its `$elems` array length IS its
     * size (the natives' convention) and `$size` is a default 0. Adopt it once — this is the one
     * place the two conventions meet.
     */
    @SuppressWarnings("unchecked")
    private Object[] elems() {
        Object u = $usrc;
        if (u != null) {
            ArrayList<E> d = (ArrayList<E>) u;
            Object[] e = d.elems();
            $size = d.$size;
            return e;
        }
        if (!$init) {
            Object[] e = $elems;
            if (e == null) {
                e = new Object[10];
                $elems = e;
                $size = 0;
            } else {
                $size = e.length;
            }
            $init = true;
            return e;
        }
        return $elems;
    }

    /** Any change of SIZE. Every marked kind refuses one. */
    private void ckSize() {
        if ($immutable != 0) {
            throw new UnsupportedOperationException();
        }
    }

    /**
     * REMOVAL. A map VIEW (kind 5) supports it — removing through `keySet()` deletes the mapping
     * — while refusing addition, so it cannot share `ckSize`. `keySet().remove(k)` threw
     * `UnsupportedOperationException` and truncated `MapViewSem` at line 7 of 58.
     */
    private void ckRemove() {
        int k = $immutable;
        if (k != 0 && k != 5) {
            throw new UnsupportedOperationException();
        }
    }

    /** Element ASSIGNMENT — allowed by `Arrays.asList` (kind 2), refused by the rest. */
    private void ckSet() {
        int k = $immutable;
        if (k == 1 || k == 3 || k == 4 || k == 5) {
            throw new UnsupportedOperationException();
        }
    }

    private void rangeCheck(int i, int n) {
        if (i < 0 || i >= n) {
            throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + n);
        }
    }

    public int size() {
        if ($usrc != null || !$init) {
            elems();
        }
        return $size;
    }

    public boolean isEmpty() {
        return size() == 0;
    }

    @SuppressWarnings("unchecked")
    public E get(int i) {
        Object[] e = ($usrc == null && $init) ? $elems : elems();
        int n = $size;
        if (i < 0 || i >= n) {
            throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + n);
        }
        return (E) e[i];
    }

    @SuppressWarnings("unchecked")
    public E set(int i, E v) {
        ckSet();
        Object[] e = elems();
        rangeCheck(i, $size);
        E old = (E) e[i];
        e[i] = v;
        return old;
    }

    /** Grow to at least `need` slots, 1.5x, keeping `$size` elements. */
    private void ensure(int need) {
        Object[] e = elems();
        if (need <= e.length) {
            return;
        }
        int cap = e.length + (e.length >> 1);
        if (cap < need) {
            cap = need;
        }
        if (cap < 10) {
            cap = 10;
        }
        Object[] b = new Object[cap];
        System.arraycopy(e, 0, b, 0, $size);
        $elems = b;
    }

    /**
     * Amortised and entirely in bytecode — no native call.
     *
     * <p>Two earlier forms are worth keeping on the record. Reallocating per append (the only
     * thing plain bytecode can do against the natives' exact-length convention) is O(n) and took
     * a boot from 40 s to **166 s**. Calling a native that pushes onto the backing Rust `Vec`
     * in place made it flat but cost **593 ns/add** — the seam this migration exists to remove,
     * paid once per element. Spare capacity plus `$size` costs neither.
     */
    public boolean add(E v) {
        if ($immutable != 0) {
            throw new UnsupportedOperationException();
        }
        // Hand-inlined: `elems()` + `ensure()` + `add()` is THREE guest calls per element and
        // measured 250-347 ns/add. A call is the expensive thing in this engine, so the small
        // helpers are worth writing out here even though they read worse — this is the one
        // method the boot runs millions of times.
        Object[] e = ($usrc == null && $init) ? $elems : elems();
        int n = $size;
        if (n == e.length) {
            int cap = n + (n >> 1);
            if (cap < 10) {
                cap = 10;
            }
            Object[] b = new Object[cap];
            System.arraycopy(e, 0, b, 0, n);
            e = b;
            $elems = b;
        }
        e[n] = v;
        $size = n + 1;
        $mod++;
        return true;
    }

    public void add(int i, E v) {
        ckSize();
        elems();
        if (i < 0 || i > $size) {
            throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + $size);
        }
        ensure($size + 1);
        Object[] e = $elems;
        System.arraycopy(e, i, e, i + 1, $size - i);
        e[i] = v;
        $size++;
        $mod++;
    }

    @SuppressWarnings("unchecked")
    public E remove(int i) {
        ckRemove();
        Object[] e = elems();
        rangeCheck(i, $size);
        E old = (E) e[i];
        System.arraycopy(e, i + 1, e, i, $size - i - 1);
        e[--$size] = null;
        $mod++;
        return old;
    }

    public boolean remove(Object o) {
        int i = indexOf(o);
        if (i < 0) {
            return false;
        }
        remove(i);
        return true;
    }

    public void clear() {
        ckRemove();
        $elems = new Object[10];
        $size = 0;
        $init = true;
        $mod++;
    }

    public int indexOf(Object o) {
        Object[] e = elems();
        int n = $size;
        if (o == null) {
            for (int i = 0; i < n; i++) {
                if (e[i] == null) {
                    return i;
                }
            }
        } else {
            for (int i = 0; i < n; i++) {
                if (o.equals(e[i])) {
                    return i;
                }
            }
        }
        return -1;
    }

    public int lastIndexOf(Object o) {
        Object[] e = elems();
        int n = $size;
        if (o == null) {
            for (int i = n - 1; i >= 0; i--) {
                if (e[i] == null) {
                    return i;
                }
            }
        } else {
            for (int i = n - 1; i >= 0; i--) {
                if (o.equals(e[i])) {
                    return i;
                }
            }
        }
        return -1;
    }

    public boolean contains(Object o) {
        return indexOf(o) >= 0;
    }

    public Object[] toArray() {
        Object[] e = elems();
        Object[] r = new Object[$size];
        System.arraycopy(e, 0, r, 0, $size);
        return r;
    }

    @SuppressWarnings("unchecked")
    public <T> T[] toArray(T[] out) {
        Object[] e = elems();
        if (out.length < $size) {
            T[] r = (T[]) java.lang.reflect.Array.newInstance(out.getClass().getComponentType(), $size);
            System.arraycopy(e, 0, r, 0, $size);
            return r;
        }
        System.arraycopy(e, 0, out, 0, $size);
        if (out.length > $size) {
            out[$size] = null;
        }
        return out;
    }

    public boolean addAll(Collection<? extends E> c) {
        ckSize();
        Object[] src = c.toArray();
        if (src.length == 0) {
            return false;
        }
        ensure($size + src.length);
        System.arraycopy(src, 0, $elems, $size, src.length);
        $size += src.length;
        $mod++;
        return true;
    }

    public boolean addAll(int i, Collection<? extends E> c) {
        ckSize();
        elems();
        if (i < 0 || i > $size) {
            throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + $size);
        }
        Object[] src = c.toArray();
        if (src.length == 0) {
            return false;
        }
        ensure($size + src.length);
        Object[] e = $elems;
        System.arraycopy(e, i, e, i + src.length, $size - i);
        System.arraycopy(src, 0, e, i, src.length);
        $size += src.length;
        $mod++;
        return true;
    }

    public boolean containsAll(Collection<?> c) {
        for (Object o : c) {
            if (!contains(o)) {
                return false;
            }
        }
        return true;
    }

    public boolean removeAll(Collection<?> c) {
        return bulkRemove(c, true);
    }

    public boolean retainAll(Collection<?> c) {
        return bulkRemove(c, false);
    }

    private boolean bulkRemove(Collection<?> c, boolean removeMatching) {
        ckRemove();
        Object[] e = elems();
        int n = $size;
        int w = 0;
        for (int i = 0; i < n; i++) {
            if (c.contains(e[i]) != removeMatching) {
                e[w++] = e[i];
            }
        }
        if (w == n) {
            return false;
        }
        for (int i = w; i < n; i++) {
            e[i] = null;
        }
        $size = w;
        $mod++;
        return true;
    }

    @SuppressWarnings("unchecked")
    public boolean removeIf(Predicate<? super E> p) {
        Object[] e = elems();
        int n = $size;
        // The CONDITIONAL kinds are why this is not just `ckSize()`. A singleton overrides
        // `removeIf` to throw whatever the predicate says; the fixed-size ones inherit the
        // default body, which only reaches its throwing primitive if something actually matches.
        int k = $immutable;
        if (k == 1 || k == 4) {
            throw new UnsupportedOperationException();
        }
        if (k != 0 && k != 5) {
            for (int i = 0; i < n; i++) {
                if (p.test((E) e[i])) {
                    throw new UnsupportedOperationException();
                }
            }
            return false;
        }
        int w = 0;
        for (int i = 0; i < n; i++) {
            if (!p.test((E) e[i])) {
                e[w++] = e[i];
            }
        }
        if (w == n) {
            return false;
        }
        for (int i = w; i < n; i++) {
            e[i] = null;
        }
        $size = w;
        $mod++;
        return true;
    }

    @SuppressWarnings("unchecked")
    public void forEach(Consumer<? super E> f) {
        Object[] e = elems();
        for (int i = 0; i < $size; i++) {
            f.accept((E) e[i]);
        }
    }

    @SuppressWarnings("unchecked")
    public void replaceAll(UnaryOperator<E> op) {
        ckSet();
        Object[] e = elems();
        for (int i = 0; i < $size; i++) {
            e[i] = op.apply((E) e[i]);
        }
    }

    @SuppressWarnings("unchecked")
    public void sort(Comparator<? super E> cmp) {
        ckSet();
        Object[] snap = toArray();
        Arrays.sort(snap, (Comparator<Object>) cmp);
        System.arraycopy(snap, 0, elems(), 0, $size);
    }

    public void ensureCapacity(int need) {
        ensure(need);
    }

    public void trimToSize() {
    }

    public Object clone() {
        ArrayList<E> c = new ArrayList<>();
        c.$elems = toArray();
        c.$size = $size;
        c.$init = true;
        return c;
    }

    @SuppressWarnings("unchecked")
    public <T> T[] toArray(java.util.function.IntFunction<T[]> gen) {
        return toArray(gen.apply(0));
    }

    // ── SequencedCollection (Java 21). `java.util.List` declares these as DEFAULT methods, so
    // javac never asks for them — but our `java/util/List` is a native interface and hands out
    // no defaults, so an overriding class must supply them itself. MC found the gap in 2.7 s:
    // `invokevirtual java/util/ArrayList.getFirst()` from `BasePressurePlateBlock`.

    public E getFirst() {
        elems();
        if ($size == 0) {
            throw new NoSuchElementException();
        }
        return get(0);
    }

    public E getLast() {
        elems();
        int n = $size;
        if (n == 0) {
            throw new NoSuchElementException();
        }
        return get(n - 1);
    }

    public void addFirst(E v) {
        add(0, v);
    }

    public void addLast(E v) {
        add(v);
    }

    public E removeFirst() {
        elems();
        if ($size == 0) {
            throw new NoSuchElementException();
        }
        return remove(0);
    }

    public E removeLast() {
        elems();
        int n = $size;
        if (n == 0) {
            throw new NoSuchElementException();
        }
        return remove(n - 1);
    }

    public List<E> reversed() {
        Object[] e = elems();
        Object[] b = new Object[$size];
        for (int i = 0; i < $size; i++) {
            b[i] = e[$size - 1 - i];
        }
        ArrayList<E> r = new ArrayList<>();
        r.$elems = b;
        r.$size = $size;
        r.$init = true;
        return r;
    }

    public Iterator<E> iterator() {
        return new Itr(0);
    }

    public ListIterator<E> listIterator() {
        return new Itr(0);
    }

    public ListIterator<E> listIterator(int i) {
        if (i < 0 || i > size()) {
            throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + size());
        }
        return new Itr(i);
    }

    /**
     * A VIEW over a range of this list, not a copy.
     *
     * <p>⛔The copy version passed all 20 differential probes and still broke MC: `subList(1,3)
     * .set(0, x)` left the parent unchanged and `subList(0,1).clear()` did not shorten it. The
     * JDK contract is that a sublist writes THROUGH, and MC's loaders rely on it.
     */
    public List<E> subList(int from, int to) {
        elems();
        if (from < 0 || to > $size || from > to) {
            throw new IndexOutOfBoundsException("subList(" + from + ", " + to + ")");
        }
        return new Sub(from, to);
    }

    private class Sub extends AbstractList<E> {
        private final int from;
        private int to;

        Sub(int from, int to) {
            this.from = from;
            this.to = to;
        }

        public int size() {
            return to - from;
        }

        public E get(int i) {
            if (i < 0 || i >= to - from) {
                throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + (to - from));
            }
            return ArrayList.this.get(from + i);
        }

        public E set(int i, E v) {
            if (i < 0 || i >= to - from) {
                throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + (to - from));
            }
            return ArrayList.this.set(from + i, v);
        }

        public void add(int i, E v) {
            if (i < 0 || i > to - from) {
                throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + (to - from));
            }
            ArrayList.this.add(from + i, v);
            to++;
        }

        public E remove(int i) {
            if (i < 0 || i >= to - from) {
                throw new IndexOutOfBoundsException("Index " + i + " out of bounds for length " + (to - from));
            }
            E old = ArrayList.this.remove(from + i);
            to--;
            return old;
        }

        public void clear() {
            for (int i = to - 1; i >= from; i--) {
                ArrayList.this.remove(i);
            }
            to = from;
        }
    }

    /**
     * ⛔`Arrays.spliterator(Object[], int, int)` is not implemented in this engine —
     * `UnsatisfiedLinkError`, which truncated `IterSem` at line 46 of 49. Go through the
     * `Arrays.asList` view instead, whose spliterator is served.
     */
    @SuppressWarnings("unchecked")
    public Spliterator<E> spliterator() {
        return Arrays.asList((E[]) toArray()).spliterator();
    }

    @SuppressWarnings("unchecked")
    public java.util.stream.Stream<E> stream() {
        return Arrays.stream((E[]) toArray());
    }

    public java.util.stream.Stream<E> parallelStream() {
        return stream();
    }

    public boolean equals(Object o) {
        if (o == this) {
            return true;
        }
        if (!(o instanceof List)) {
            return false;
        }
        List<?> other = (List<?>) o;
        Object[] e = elems();
        if (other.size() != $size) {
            return false;
        }
        for (int i = 0; i < $size; i++) {
            Object x = e[i];
            Object y = other.get(i);
            if (x == null ? y != null : !x.equals(y)) {
                return false;
            }
        }
        return true;
    }

    public int hashCode() {
        Object[] e = elems();
        int h = 1;
        for (int i = 0; i < $size; i++) {
            h = 31 * h + (e[i] == null ? 0 : e[i].hashCode());
        }
        return h;
    }

    public String toString() {
        Object[] e = elems();
        if ($size == 0) {
            return "[]";
        }
        StringBuilder sb = new StringBuilder();
        sb.append('[');
        for (int i = 0; i < $size; i++) {
            if (i > 0) {
                sb.append(", ");
            }
            sb.append(e[i] == this ? "(this Collection)" : String.valueOf(e[i]));
        }
        return sb.append(']').toString();
    }

    private class Itr implements ListIterator<E> {
        int i;
        int last = -1;
        int exp = $mod;

        Itr(int start) {
            i = start;
        }

        private void ck() {
            if ($mod != exp) {
                throw new ConcurrentModificationException();
            }
        }

        public boolean hasNext() {
            return i < ArrayList.this.size();
        }

        @SuppressWarnings("unchecked")
        public E next() {
            ck();
            Object[] e = ($usrc == null && $init) ? $elems : elems();
            if (i >= $size) {
                throw new NoSuchElementException();
            }
            last = i;
            return (E) e[i++];
        }

        public boolean hasPrevious() {
            return i > 0;
        }

        @SuppressWarnings("unchecked")
        public E previous() {
            if (i <= 0) {
                throw new NoSuchElementException();
            }
            last = --i;
            return (E) elems()[i];
        }

        public int nextIndex() {
            return i;
        }

        public int previousIndex() {
            return i - 1;
        }

        public void remove() {
            ckRemove();
            ck();
            if (last < 0) {
                throw new IllegalStateException();
            }
            ArrayList.this.remove(last);
            if (last < i) {
                i--;
            }
            last = -1;
            exp = $mod;
        }

        public void set(E v) {
            ckSet();
            if (last < 0) {
                throw new IllegalStateException();
            }
            ArrayList.this.set(last, v);
        }

        public void add(E v) {
            ckSize();
            ck();
            ArrayList.this.add(i++, v);
            last = -1;
            exp = $mod;
        }
    }
}
