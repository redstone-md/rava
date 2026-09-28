// COST BY CALL KIND, same trivial body, same loop shape. FrameBench puts a static
// monomorphic call at 1.1 ns/call; the JDK-bytecode experiment put `List.size` (an
// INTERFACE call on a heap-Ref receiver) at 334 ns. If that gap is the call kind rather
// than the callee, it is the largest single fact left in this engine.
public class CallKinds {
    interface I { int v(); }
    static class A implements I { int f = 3; public int v(){ return f; } }
    static class B implements I { int f = 4; public int v(){ return f; } }
    static class C implements I { int f = 5; public int v(){ return f; } }
    static class D implements I { int f = 6; public int v(){ return f; } }
    static int sf = 3;
    static A a = new A(); static I i1 = new A();
    static I[] poly = { new A(), new B(), new C(), new D() };
    static A[] mono = { new A(), new A(), new A(), new A() };

    static int stat(){ return sf; }
    static long statLoop(int n){ long r=0; for(int k=0;k<n;k++) r+=stat(); return r; }
    static long virtMono(int n){ long r=0; for(int k=0;k<n;k++) r+=a.v(); return r; }
    static long virtArr(int n) { long r=0; for(int k=0;k<n;k++) r+=mono[k&3].v(); return r; }
    static long ifaceMono(int n){ long r=0; for(int k=0;k<n;k++) r+=i1.v(); return r; }
    static long ifacePoly(int n){ long r=0; for(int k=0;k<n;k++) r+=poly[k&3].v(); return r; }
    static long control(int n)  { long r=0; for(int k=0;k<n;k++) r+=(k&3); return r; }
    static long run(int w,int n){ switch(w){
        case 0:return statLoop(n); case 1:return virtMono(n); case 2:return virtArr(n);
        case 3:return ifaceMono(n); case 4:return ifacePoly(n); default:return control(n); } }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/call"); return acc; }
    public static void main(String[] s){
        int n=300000, reps=10; long acc=0;
        String[] nm={"static        ","virtual mono  ","virtual array ","interface mono","interface poly","control       "};
        for(int w=0;w<6;w++) acc+=time(nm[w],w,n,reps);
        System.out.println("acc="+acc);
    }
}
