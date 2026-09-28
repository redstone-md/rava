// PROFILE TARGET: a virtual call whose only difference from a 11 ns one is that it
// returns a REFERENCE. 448 ns/op against the JVM's 0.
public class RefRet {
    static class P { final String v = "x"; String s(){ return v; } int i(){ return 1; } }
    static P p = new P(); static Object sink; static int isink;
    static long refs(int n){ long r=0; for(int k=0;k<n;k++){ sink=p.s(); r++; } return r; }
    public static void main(String[] a){
        long acc=0; for(int w=0;w<3;w++) acc+=refs(200000);
        long t0=System.nanoTime(); for(int r=0;r<25;r++) acc+=refs(200000); long t1=System.nanoTime();
        System.out.println("refRet "+((t1-t0)/(200000L*25))+" ns/op acc="+acc+" "+sink);
    }
}
