import java.util.*;
import java.util.function.*;
import java.util.stream.*;

// λ-PIC correctness matrix: dispatch shapes, lambdas/method-refs, inheritance/
// default methods, invokedynamic concat, boxing/primitive streams, exceptions
// across dispatch. Deterministic per-category checksums.
public class DispatchMatrix {
    interface Shape { double area(); default String kind(){ return "shape"; } }
    static class Circle implements Shape { double r; Circle(double r){this.r=r;} public double area(){return r*r*3;} public String kind(){return "circle";} }
    static class Sq implements Shape { double s; Sq(double s){this.s=s;} public double area(){return s*s;} }
    static class Tri implements Shape { double b,h; Tri(double b,double h){this.b=b;this.h=h;} public double area(){return b*h/2;} public String kind(){return "tri";} }
    static class Rect implements Shape { double w,h; Rect(double w,double h){this.w=w;this.h=h;} public double area(){return w*h;} }

    public static void main(String[] a) {
        // 1. mono/poly/megamorphic virtual+interface dispatch
        List<Shape> shapes = new ArrayList<>();
        for (int i=0;i<1200;i++){ int k=i%4; shapes.add(k==0?new Circle(i%7):k==1?new Sq(i%5):k==2?new Tri(i%6,i%3):new Rect(i%4,i%9)); }
        long d1=0; for (Shape s: shapes){ d1 += (long)s.area(); d1 ^= s.kind().hashCode(); }
        System.out.println("C1_dispatch=" + d1);

        // 2. non-capturing + capturing lambdas + method refs
        Function<Integer,Integer> sq = x -> x*x;                 // non-capturing
        int cap = 7; Function<Integer,Integer> add = x -> x+cap; // capturing
        Function<Integer,String> ref = String::valueOf;          // method ref
        long d2=0; for (int i=0;i<1000;i++){ d2 += sq.apply(i%50); d2 += add.apply(i%50); d2 ^= ref.apply(i).hashCode(); }
        System.out.println("C2_lambdas=" + d2);

        // 3. default methods + overridden through interface ref, megamorphic
        long d3=0; BiFunction<Shape,Integer,Long> f = (s,i)-> (long)(s.area()) + s.kind().length()*i;
        for (int i=0;i<1000;i++){ d3 += f.apply(shapes.get(i%shapes.size()), i%13); }
        System.out.println("C3_default=" + d3);

        // 4. invokedynamic string concat (mixed types)
        long d4=0; for (int i=0;i<1000;i++){ String s = "x"+i+"-"+(i*1.0)+"-"+(i%2==0); d4 ^= s.hashCode(); }
        System.out.println("C4_concat=" + d4);

        // 5. boxing/unboxing + primitive streams (Int/Long/Double)
        long is = IntStream.range(0,1500).map(x->x*2).sum();
        long ls = IntStream.range(0,1500).mapToLong(x->(long)x*3).sum();
        double ds = IntStream.range(0,1500).mapToDouble(x->x*0.5).sum();
        System.out.println("C5_streams=" + is + "," + ls + "," + (long)ds);

        // 6. exceptions across dispatch boundary
        long d6=0; Function<Integer,Integer> risky = x -> { if (x%100==0) throw new RuntimeException("e"); return x; };
        for (int i=1;i<=1000;i++){ try { d6 += risky.apply(i); } catch(RuntimeException e){ d6 ^= e.getMessage().hashCode(); } }
        System.out.println("C6_exc=" + d6);

        // 7. reflection / dynamic linkage
        long d7=0; try { Class<?> c = Class.forName("java.lang.Integer"); d7 = c.getName().hashCode(); } catch(Exception e){ d7=-1; }
        System.out.println("C7_reflect=" + d7);
        System.out.println("DONE");
    }
}
