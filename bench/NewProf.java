// PROFILE TARGET: `new` on a tiny guest class. 238 ns/op against the JVM's 3, and allocation
// is the boot's single largest host row (mi_malloc 4.4% + mi_free 0.7% + gc 2.2%).
public class NewProf {
    static class P { int a,b; P(int a,int b){this.a=a;this.b=b;} }
    static Object sink;
    static long mk(int n){ long r=0; for(int k=0;k<n;k++){ sink=new P(k,1); r++; } return r; }
    public static void main(String[] a){
        long acc=0; for(int w=0;w<3;w++) acc+=mk(200000);
        long t0=System.nanoTime(); for(int r=0;r<25;r++) acc+=mk(200000); long t1=System.nanoTime();
        System.out.println("new "+((t1-t0)/(200000L*25))+" ns/op acc="+acc);
    }
}
