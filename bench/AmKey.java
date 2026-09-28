// OBJECT-KEYED LOOKUP under RAVA_ARENA_MAPS. The registry build keys
// Map<ResourceKey<?>, ...> by VALUE-equal but distinct instances; if put and get derive
// different keys, entries register and then fail to be found — which is exactly what
// "Errors during registry creation" reports.
import java.util.*;
public class AmKey {
    record K(String ns, String path) {}
    static final class C {
        final int a; final String b;
        C(int a, String b){ this.a=a; this.b=b; }
        @Override public boolean equals(Object o){ return o instanceof C c && c.a==a && c.b.equals(b); }
        @Override public int hashCode(){ return a*31 + b.hashCode(); }
        @Override public String toString(){ return "C("+a+","+b+")"; }
    }
    public static void main(String[] x){
        Map<K,String> rm = new HashMap<>();
        rm.put(new K("mc","stone"), "S");
        System.out.println("record  get-equal=" + rm.get(new K("mc","stone")) + " contains=" + rm.containsKey(new K("mc","stone")));
        Map<C,String> cm = new HashMap<>();
        cm.put(new C(1,"x"), "V");
        System.out.println("class   get-equal=" + cm.get(new C(1,"x")) + " contains=" + cm.containsKey(new C(1,"x")));
        Map<Object,String> mixed = new HashMap<>();
        for (int i=0;i<40;i++) mixed.put(new C(i,"k"+i), "v"+i);
        int found=0; for (int i=0;i<40;i++) if (mixed.get(new C(i,"k"+i))!=null) found++;
        System.out.println("bulk    found=" + found + "/40 size=" + mixed.size());
        Map<Object,String> ident = new IdentityHashMap<>();
        C c1 = new C(2,"y");
        ident.put(c1,"I");
        System.out.println("identity same=" + ident.get(c1) + " equalOther=" + ident.get(new C(2,"y")));
        Set<C> hs = new HashSet<>();
        hs.add(new C(3,"z"));
        System.out.println("set     contains-equal=" + hs.contains(new C(3,"z")) + " size=" + hs.size());
    }
}
