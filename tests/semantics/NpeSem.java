// What the JVM's HELPFUL NullPointerException actually says, shape by shape.
//
// DIVERGES: monitor
//
// (`monitor` is the only line that still differs, and it is ThreadSem's declared limit
// rather than this probe's: monitorenter now throws the JVM's NullPointerException on a
// null, but `LOCK.wait(1)` outside a synchronized block still cannot raise
// IllegalMonitorStateException, because there is no monitor ownership to check.)
//
// bench/ExcSem.java diverged on one line — this engine says
// `java/lang/String.length` where the JVM says
// `Cannot invoke "String.length()" because "<local0>" is null` — and the format cannot be
// guessed from one example. Does it qualify the class? What does it call a field, an array
// element, a method result? Which shapes get a "because" clause at all?
//
// So this asks the reference implementation, one shape per line. Compiled WITHOUT -g on
// purpose: the jar this engine runs is shipped without a LocalVariableTable, and the JVM
// falls back to `<localN>` exactly then. Compiling with debug info would answer a question
// nobody is asking.
import java.util.*;

public class NpeSem {
    static class Holder { int x; String s; }

    static String sfield;
    static int[] sarr;
    static Map<String, String> smap;

    static String msg(Runnable r) {
        try { r.run(); return "NO THROW"; }
        catch (NullPointerException e) { return String.valueOf(e.getMessage()); }
        catch (Throwable t) { return t.getClass().getName(); }
    }

    static String giveNull() { return null; }

    public static void main(String[] a) {
        // invoke on a null LOCAL, java.lang type
        System.out.println("local.String    " + msg(() -> { String s = null; s.length(); }));
        // invoke on a null LOCAL, non-java.lang type
        System.out.println("local.List      " + msg(() -> { List<String> l = null; l.size(); }));
        // invoke on a null STATIC FIELD
        System.out.println("static.field    " + msg(() -> sfield.length()));
        // invoke on the RESULT of a call that returned null
        System.out.println("call.result     " + msg(() -> giveNull().length()));
        // read a FIELD through a null reference
        System.out.println("getfield        " + msg(() -> { NpeSem o = null; System.out.print(o.hashCode()); }));
        // array LENGTH of null
        System.out.println("arraylength     " + msg(() -> { int[] x = null; System.out.print(x.length); }));
        // array LOAD from null
        System.out.println("arrayload       " + msg(() -> { int[] x = null; System.out.print(x[0]); }));
        // array STORE into null
        System.out.println("arraystore      " + msg(() -> { int[] x = null; x[0] = 1; }));
        // array load from a null STATIC array
        System.out.println("static.arrayload " + msg(() -> System.out.print(sarr[0])));
        // interface call on a null static
        System.out.println("static.iface    " + msg(() -> smap.get("k")));
        // unboxing a null
        System.out.println("unbox           " + msg(() -> { Integer i = null; System.out.print(i + 1); }));
        // WHICH CLASSES GET QUALIFIED. `String.length()` came back unqualified while
        // `java.lang.Integer.intValue()` did not, and both owners are in java.lang — so the
        // rule is not "strip java.lang". These pin it down.
        System.out.println("q.Object        " + msg(() -> { Object o = null; o.toString(); }));
        System.out.println("q.Integer       " + msg(() -> { Integer i = null; i.intValue(); }));
        System.out.println("q.StringBuilder " + msg(() -> { StringBuilder b = null; b.length(); }));
        System.out.println("q.Thread        " + msg(() -> { Thread t = null; t.getName(); }));
        System.out.println("q.CharSequence  " + msg(() -> { CharSequence c = null; c.length(); }));
        System.out.println("q.String2       " + msg(() -> { String s = null; s.isEmpty(); }));
        System.out.println("q.NpeSem        " + msg(() -> { NpeSem n = null; n.toString(); }));
        // PARAMETER rendering in the signature.
        System.out.println("p.twoArgs       " + msg(() -> { Map<String,String> m = null; m.put("a","b"); }));
        System.out.println("p.prim          " + msg(() -> { List<String> l = null; l.get(0); }));
        System.out.println("p.array         " + msg(() -> { String s = null; s.getBytes(); }));
        // FIELD access through null, read and write, and an OBJECT array.
        System.out.println("f.read          " + msg(() -> { Holder h = null; System.out.print(h.x); }));
        System.out.println("f.write         " + msg(() -> { Holder h = null; h.x = 1; }));
        System.out.println("f.readRef       " + msg(() -> { Holder h = null; System.out.print(h.s); }));
        System.out.println("a.objload       " + msg(() -> { String[] x = null; System.out.print(x[0]); }));
        System.out.println("a.objstore      " + msg(() -> { String[] x = null; x[0] = "a"; }));
        System.out.println("a.longload      " + msg(() -> { long[] x = null; System.out.print(x[0]); }));
        System.out.println("a.chained       " + msg(() -> { int[][] x = new int[1][]; System.out.print(x[0][0]); }));
        // synchronized on null
        System.out.println("monitor         " + msg(() -> { Object o = null; synchronized (o) { } }));
        // throw null
        System.out.println("throw.null      " + msg(() -> { throw (RuntimeException) null; }));
        // an explicitly constructed NPE keeps its own message
        System.out.println("explicit        " + msg(() -> { throw new NullPointerException("mine"); }));
        // and one with no message at all
        System.out.println("explicit.empty  " + msg(() -> { throw new NullPointerException(); }));
    }
}
