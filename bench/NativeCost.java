// WHAT DOES ONE NATIVE BRIDGE CALL COST? — the number block A's 8.26M cascade entries over
// 8.3 s of NATIVE_BRIDGES implies is ~1 us, which would be two orders off HotSpot. A division
// is not a measurement; this is.
//
// ⚠**EACH LOOP LIVES IN ITS OWN METHOD, CALLED TWICE.** The first version put them all in
// `main`, and its control loop read 44 ns/iter — `main` runs once, so nothing tiered it up and
// every number was an INTERPRETED number wearing a product label. A method called repeatedly
// reaches the compiled tier by entry count and needs no OSR.
// ⚠**THE STORED VALUE MATTERS.** The first version stored `Integer.valueOf(i)`, and rava boxes
// `Integer` as a raw `Value::Int` — so every `get` had to materialize an `Integer` OBJECT to
// hand a reference back to compiled code, and that allocation was inside the `Map.get` number.
// MC's block-state maps store objects, so the values here are objects too.
import java.util.*;

public class NativeCost {
    static Object[] keys = new Object[4];
    static HashMap<Object, Object> m = new HashMap<>();
    static ArrayList<Object> list = new ArrayList<>();
    // CONTROL FOR THE DIAGNOSIS: `Integer` keys take rava's alloc-free `MapKey::Int` path.
    // If they are fast where bare `Object` keys are not, the cost is the KEY DERIVATION, not
    // the bridge.
    static HashMap<Object, Object> mi = new HashMap<>();
    static Integer[] ikeys = new Integer[4];
    static Object x, y;

    static long mapGet(int n)      { long a = 0; for (int i = 0; i < n; i++) a += m.get(keys[i & 3]) == null ? 0 : 1; return a; }
    static long objEquals(int n)   { long a = 0; for (int i = 0; i < n; i++) a += Objects.equals(x, y) ? 1 : 0; return a; }
    static long listSize(int n)    { long a = 0; for (int i = 0; i < n; i++) a += list.size(); return a; }
    static long containsKey(int n) { long a = 0; for (int i = 0; i < n; i++) a += m.containsKey(keys[i & 3]) ? 1 : 0; return a; }
    static long mapGetInt(int n)   { long a = 0; for (int i = 0; i < n; i++) a += mi.get(ikeys[i & 3]) == null ? 0 : 1; return a; }
    // The ops `bench/StreamApplyCost.java` is built from, priced WITHOUT `bridgestat`'s per-call
    // mutex+hashmap accounting — which inflates a HIGH-COUNT native far more than a low-count one
    // and therefore biases its ranking, not merely its absolute numbers.
    static long listIter(int n)  { long a = 0; for (int i = 0; i < n; i++) { java.util.Iterator<Object> it = list.iterator(); if (it.hasNext()) a++; } return a; }
    static long listAdd(int n)   { long a = 0; for (int i = 0; i < n; i++) { ArrayList<Object> t = new ArrayList<>(); t.add(x); a += t.size(); } return a; }
    static long listGet(int n)   { long a = 0; for (int i = 0; i < n; i++) a += list.get(i & 3) == null ? 0 : 1; return a; }
    // Split of `List.add(new)`: is 3.5 us the allocation or the add?
    static long listNew(int n)   { long a = 0; for (int i = 0; i < n; i++) { ArrayList<Object> t = new ArrayList<>(); a += t.size(); } return a; }
    static long listAdd1(int n)  { long a = 0; for (int i = 0; i < n; i++) { list.add(x); list.remove(list.size() - 1); a++; } return a; }
    // Is 2.4 us the ALLOCATOR or the collection birth path? A plain object allocates on the same
    // heap through the same `new`, and touches no native.
    // THE CATEGORY THE MODULE PROFILE NAMES: call and dispatch is 36.4% of the host
    // (`vmstate` 25.8 + jit 3.6 + tlcache 4.3 + frames 1.9 + owner_bits 0.8), and the price list
    // had no entry for a plain guest call. `sink1` defeats inlining-to-nothing without adding
    // work: the callee returns its argument.
    static long callee(long v)     { return v; }
    long ivcallee(long v)          { return v; }
    static NativeCost inst = new NativeCost();
    static long callStatic(int n)  { long a = 0; for (int i = 0; i < n; i++) a += callee(i & 3); return a; }
    static long callVirtual(int n) { long a = 0; for (int i = 0; i < n; i++) a += inst.ivcallee(i & 3); return a; }
    // A CAPTURING lambda — 99.8% of the 4.3 M LambdaMetafactory assemblies a boot are these.
    // The object birth is required (Java allocates too); everything above a plain `new Object`
    // is assembly MACHINERY, and machinery is cacheable per site even when the object is not.
    interface LF { long f(long v); }
    static long makeLambda(int n) { long a = 0; for (int i = 0; i < n; i++) { long c = i & 3; LF f = v -> v + c; a += f.f(1); } return a; }
    static Object keep;
    static long newObject(int n)  { long a = 0; for (int i = 0; i < n; i++) { keep = new Object(); a++; } return a; }
    static long newPlain(int n)   { long a = 0; for (int i = 0; i < n; i++) { keep = new java.util.concurrent.atomic.AtomicLong(i); a++; } return a; }
    // CONTROL FOR THE ALLOCATION NUMBER: the same putstatic, no allocation. The difference
    // between this and `newObject` is what one object birth costs, with nothing else in it.
    static long storeOnly(int n)  { long a = 0; for (int i = 0; i < n; i++) { keep = x; a++; } return a; }
    static long loadOnly(int n)   { long a = 0; for (int i = 0; i < n; i++) { if (keep != null) a++; } return a; }
    static int ikeep;
    static long storeInt(int n)   { long a = 0; for (int i = 0; i < n; i++) { ikeep = i; a++; } return a; }
    static long control(int n)     { long a = 0; for (int i = 0; i < n; i++) a += (i & 3); return a; }

    static long time(String tag, int which, int n, int reps) {
        long acc = 0;
        // WARM: the same method, the same shape, discarded. Then measure.
        for (int r = 0; r < reps; r++) acc += run(which, n);
        long t0 = System.nanoTime();
        for (int r = 0; r < reps; r++) acc += run(which, n);
        long t1 = System.nanoTime();
        System.out.println(tag + " " + ((t1 - t0) / ((long) n * reps)) + " ns/call");
        return acc;
    }

    static long run(int which, int n) {
        switch (which) {
            case 0: return mapGet(n);
            case 1: return objEquals(n);
            case 2: return listSize(n);
            case 3: return containsKey(n);
            case 5: return mapGetInt(n);
            case 6: return listIter(n);
            case 7: return listAdd(n);
            case 8: return listGet(n);
            case 9: return listNew(n);
            case 10: return listAdd1(n);
            case 11: return newObject(n);
            case 12: return newPlain(n);
            case 13: return storeOnly(n);
            case 16: return callStatic(n);
            case 17: return callVirtual(n);
            case 18: return makeLambda(n);
            case 14: return loadOnly(n);
            case 15: return storeInt(n);
            default: return control(n);
        }
    }

    public static void main(String[] a) {
        int n = Integer.parseInt(a.length > 0 ? a[0] : "50000");
        int reps = Integer.parseInt(a.length > 1 ? a[1] : "40");
        for (int i = 0; i < 4; i++) { keys[i] = new Object(); m.put(keys[i], keys[i]); list.add(keys[i]);
                                      ikeys[i] = Integer.valueOf(1000 + i); mi.put(ikeys[i], keys[i]); }
        x = keys[1]; y = keys[2];
        long acc = 0;
        // A THIRD ARG PINS ONE CASE, so a profile of this process is unambiguous about which
        // call it is attributing.
        if (a.length > 2) {
            int only = Integer.parseInt(a[2]);
            String[] nm = {"Map.get        ", "Objects.equals ", "List.size      ", "Map.containsKey",
                           "control loop   ", "Map.get(Integer)", "List.iterator  ", "List.add(new)  ",
                           "List.get       ", "new ArrayList  ", "add+remove     ", "new Object     ", "new AtomicLong ", "putstatic only ", "getstatic ref  ", "putstatic int  ", "call static    ", "call virtual   ", "capturing lambda"};
            System.out.println("acc=" + time(nm[only], only, n, reps));
            return;
        }
        acc += time("Map.get        ", 0, n, reps);
        acc += time("Objects.equals ", 1, n, reps);
        acc += time("List.size      ", 2, n, reps);
        acc += time("Map.containsKey", 3, n, reps);
        acc += time("Map.get(Integer)", 5, n, reps);
        acc += time("List.iterator  ", 6, n, reps);
        acc += time("List.add(new)  ", 7, n, reps);
        acc += time("List.get       ", 8, n, reps);
        acc += time("new ArrayList  ", 9, n, reps);
        acc += time("new Object     ", 11, n, reps);
        acc += time("capturing lambda", 18, n, reps);
        acc += time("call static    ", 16, n, reps);
        acc += time("call virtual   ", 17, n, reps);
        acc += time("putstatic only ", 13, n, reps);
        acc += time("getstatic ref  ", 14, n, reps);
        acc += time("putstatic int  ", 15, n, reps);
        acc += time("new AtomicLong ", 12, n, reps);
        acc += time("add+remove     ", 10, n, reps);
        acc += time("control loop   ", 4, n, reps);
        System.out.println("acc=" + acc);
    }
}
