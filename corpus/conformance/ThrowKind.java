public class ThrowKind {
    static int litThrow(int i){ throw new RuntimeException("LITERAL"); }      // compiled thrower, literal msg
    static int concatThrow(int i){ throw new RuntimeException("C"+(i%3)); }   // compiled thrower, concat msg
    static int varThrow(int i, String m){ throw new RuntimeException(m); }    // compiled thrower, arg msg
    static String P(int i, int kind){
        try { if(kind==0) litThrow(i); else if(kind==1) concatThrow(i); else varThrow(i,"V"+(i%3)); return "x"; }
        catch(RuntimeException e){ return String.valueOf(e.getMessage()); }
    }
    public static void main(String[] a){
        for(int i=0;i<6000;i++){ String l=P(i,0),c=P(i,1),v=P(i,2); if(i==5999) System.out.println("late: lit="+l+" concat="+c+" var="+v); }
        System.out.println("DONE");
    }
}
