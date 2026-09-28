import java.util.function.*;
public class ExcRepro {
    public static void main(String[] a) {
        Function<Integer,Integer> risky = x -> { if (x%100==0) throw new RuntimeException("e"); return x; };
        long d = 0;
        for (int i=1;i<=300;i++){ try { d += risky.apply(i); } catch(RuntimeException e){ d ^= e.getMessage().hashCode(); } }
        System.out.println("EXC_lambda=" + d);
        // control: exception from a normal method call in a loop
        long d2 = 0;
        for (int i=1;i<=300;i++){ try { d2 += risky2(i); } catch(RuntimeException e){ d2 ^= 7; } }
        System.out.println("EXC_method=" + d2);
        System.out.println("DONE");
    }
    static int risky2(int x){ if (x%100==0) throw new RuntimeException("m"); return x; }
}
