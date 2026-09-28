public class PicShapes {
    interface S { int v(); default int d(){ return v()+1; } }
    static class A implements S { public int v(){return 1;} }
    static class B implements S { public int v(){return 2;} }
    static class C implements S { public int v(){return 3;} }
    static class D implements S { public int v(){return 4;} }
    static class E implements S { public int v(){return 5;} }
    static class F implements S { public int v(){return 6;} public int d(){ return 100; } }
    static long run(S[] arr, int n){ long a=0; for(int i=0;i<n;i++){ S s=arr[i%arr.length]; a+=s.v(); a+=s.d(); } return a; }
    public static void main(String[] x){
        S[] mono={new A()}; S[] poly={new A(),new B(),new C()}; S[] mega={new A(),new B(),new C(),new D(),new E(),new F()};
        System.out.println("mono="+run(mono,5000));
        System.out.println("poly="+run(poly,5000));
        System.out.println("mega="+run(mega,5000));
        // null receiver NPE
        long np=0; for(int i=0;i<3000;i++){ try{ S s=(i%500==0)?null:mono[0]; np+=s.v(); }catch(NullPointerException e){ np^=7; } }
        System.out.println("nullrecv="+np);
        // alternation / identity reuse
        long alt=0; S ra=new A(), rb=new B(); for(int i=0;i<5000;i++){ S s=(i&1)==0?ra:rb; alt+=s.v()*s.d(); }
        System.out.println("alt="+alt);
        System.out.println("DONE");
    }
}
