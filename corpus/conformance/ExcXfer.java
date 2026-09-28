public class ExcXfer {
    static int inner(int i){ throw new RuntimeException("MSG"+(i%3)); }
    static String probe(int i){
        try { inner(i); return "noexc"; }
        catch(RuntimeException e){
            String cls = (e==null)?"NULL":e.getClass().getName();
            String msg = (e==null)?"NULL":String.valueOf(e.getMessage());
            return cls+"|"+msg;
        }
    }
    public static void main(String[] a){
        // print first, a middle, and late iteration to see compile-state transition
        for (int i=0;i<6000;i++){ String r=probe(i); if(i<3 || i==2999 || i==5999) System.out.println("i="+i+" "+r); }
        System.out.println("DONE");
    }
}
