public class ClassLoad {
    static class X { static int V; static { V = compute(); } static int compute(){ return 42; } int get(){ return V; } }
    static class Y extends X { int gy(){ return get()+1; } }
    public static void main(String[] a){
        long h=0;
        for(int i=0;i<4000;i++){ Y y=new Y(); h+=y.gy(); h+=X.V; }
        System.out.println("h="+h);
        // late linkage: first touch of Z triggers its clinit here
        System.out.println("late="+Z.tag());
        System.out.println("DONE");
    }
    static class Z { static final String T; static { T = "Z"+ (7*6); } static String tag(){ return T; } }
}
