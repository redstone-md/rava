// PROFILE TARGET: a lambda invocation. 908 ns/op against the JVM's 0, and MC's reload phase
// is codec decoding — lambdas end to end.
import java.util.function.IntUnaryOperator;
public class LamProf {
    static int[] sink = new int[1];
    static long go(int n){ IntUnaryOperator f = x -> x + 1; long r=0; for(int k=0;k<n;k++) r+=f.applyAsInt(k); return r; }
    public static void main(String[] a){
        long acc=0; for(int w=0;w<3;w++) acc+=go(100000);
        long t0=System.nanoTime(); for(int r=0;r<20;r++) acc+=go(100000); long t1=System.nanoTime();
        System.out.println("lambda "+((t1-t0)/(100000L*20))+" ns/op acc="+acc);
    }
}
