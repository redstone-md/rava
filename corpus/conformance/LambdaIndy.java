import java.util.function.*;
public class LambdaIndy {
    static int sfn(int x){ return x*3; }
    static class Bx { int b; Bx(int b){this.b=b;} int add(int x){return b+x;} }
    static Function<Integer,Function<Integer,Integer>> adder(){ return a -> b -> a+b; } // lambda returning lambda
    public static void main(String[] z){
        int cap=11;
        Function<Integer,Integer> nonCap = x->x+1;
        Function<Integer,Integer> capF = x->x+cap;
        IntUnaryOperator sref = LambdaIndy::sfn;            // static method ref
        Bx bx=new Bx(100); IntUnaryOperator bref = bx::add; // bound instance ref
        Function<String,Integer> uref = String::length;     // unbound instance ref
        Supplier<Bx> cref = () -> new Bx(7);                // ctor-ish via lambda
        Function<Integer,Function<Integer,Integer>> nest = adder();
        long a=0;
        for(int i=0;i<5000;i++){
            a += nonCap.apply(i%10); a += capF.apply(i%10);
            a += sref.applyAsInt(i%10); a += bref.applyAsInt(i%10);
            a += uref.apply("s"+(i%10)); a += cref.get().b;
            a += nest.apply(i%10).apply(3);
        }
        System.out.println("lam="+a);
        // lambda throwing across native/dispatch
        long e=0; Function<Integer,Integer> risky=x->{ if(x%100==0) throw new IllegalStateException("L"+(x%3)); return x; };
        for(int i=1;i<=4000;i++){ try{ e+=risky.apply(i); }catch(IllegalStateException ex){ e^=ex.getMessage().hashCode(); } }
        System.out.println("lamthrow="+e);
        System.out.println("DONE");
    }
}
