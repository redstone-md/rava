// Systematic exception-transfer bisection. Each case runs its throw/catch inside
// a hot (JIT-compiled) method; results fold into null-safe per-case accumulators
// so a divergence shows as a different checksum rather than aborting the run.
public class ExcMatrix {
    static long H(String s){ return s == null ? -1L : s.hashCode(); }

    static int cA(int i){ try { throw new RuntimeException("m"+(i%3)); } catch(RuntimeException e){ return e!=null?1:0; } }
    static String cB(int i){ try { throw new RuntimeException("m"+(i%3)); } catch(RuntimeException e){ return e.getClass().getName(); } }
    static String cC(int i){ try { throw new RuntimeException("m"+(i%3)); } catch(RuntimeException e){ return e.getMessage(); } }
    static boolean cD(int i){ try { throw new RuntimeException("m"+(i%3)); } catch(RuntimeException e){ return e.getCause()==null; } }
    static String cE(int i){ try { try { throw new RuntimeException("m"+(i%3)); } catch(RuntimeException e){ throw e; } } catch(RuntimeException e2){ return e2.getMessage(); } }
    static String cF(int i){ String r="?"; try { if((i&1)==0) throw new IllegalStateException("F"+(i%3)); } catch(IllegalStateException e){ r=e.getMessage(); } return r; }
    static int inner(int i){ if(i>=0) throw new RuntimeException("G"+(i%3)); return 0; }        // interpreted-ish callee
    static String cG(int i){ try { return ""+inner(i); } catch(RuntimeException e){ return e.getMessage(); } }
    static int innerC(int i){ try { throw new RuntimeException("H"+(i%3)); } finally { } }         // callee always throws
    static String cH(int i){ try { return ""+innerC(i); } catch(RuntimeException e){ return e.getMessage(); } }
    static String cI(int i){ String r="0"; try { try { throw new RuntimeException("I"+(i%3)); } finally { r="fin"; } } catch(RuntimeException e){ r=r+"/"+e.getMessage(); } return r; }
    static String cJ(int i){ try { throw new IllegalArgumentException("J"+(i%3)); } catch(RuntimeException e){ return e.getClass().getSimpleName()+":"+e.getMessage(); } }
    static String cK(int i){ try { throw new RuntimeException(); } catch(RuntimeException e){ return String.valueOf(e.getMessage()); } } // null message

    public static void main(String[] a){
        long A=0,B=0,C=0,D=0,E=0,F=0,G=0,Hc=0,I=0,J=0,K=0;
        for (int i=0;i<8000;i++){
            A+=cA(i); B^=H(cB(i)); C^=H(cC(i)); D+=cD(i)?1:0; E^=H(cE(i));
            F^=H(cF(i)); G^=H(cG(i)); Hc^=H(cH(i)); I^=H(cI(i)); J^=H(cJ(i)); K^=H(cK(i));
        }
        System.out.println("A_nonnull="+A);
        System.out.println("B_class="+B);
        System.out.println("C_msg="+C);
        System.out.println("D_causeNull="+D);
        System.out.println("E_rethrow="+E);
        System.out.println("F_directThrow="+F);
        System.out.println("G_interpCallee="+G);
        System.out.println("H_compiledCallee="+Hc);
        System.out.println("I_finally="+I);
        System.out.println("J_superCatch="+J);
        System.out.println("K_nullMsg="+K);
        System.out.println("DONE");
    }
}
