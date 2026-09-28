// PROFILE TARGET: entering a try block. 175 ns/op against the JVM's 1, and the body inside is
// a 3 ns array read — so the try itself is ~170 ns. Zero-cost EH is the JVM's baseline.
public class TryProf {
    static int[] ia = new int[64];
    static long go(int n){ long r=0; for(int k=0;k<n;k++){ try { r+=ia[k&63]; } catch(RuntimeException e){ r--; } } return r; }
    public static void main(String[] a){
        long acc=0; for(int w=0;w<3;w++) acc+=go(200000);
        long t0=System.nanoTime(); for(int r=0;r<25;r++) acc+=go(200000); long t1=System.nanoTime();
        System.out.println("try "+((t1-t0)/(200000L*25))+" ns/op acc="+acc);
    }
}
