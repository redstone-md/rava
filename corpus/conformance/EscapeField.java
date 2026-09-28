public class EscapeField {
    static class Box { String s; }
    static Box makeLit(int i){ Box b = new Box(); b.s = "LIT"; return b; }        // literal into field, escapes via return
    static Box makeCon(int i){ Box b = new Box(); b.s = "C"+(i%3); return b; }     // concat into field
    static Box makePar(int i, String m){ Box b = new Box(); b.s = m; return b; }   // param into field
    static String[] arr(int i){ String[] a = new String[2]; a[0]="AL"; a[1]="A"+(i%3); return a; } // array element escape
    public static void main(String[] a){
        long h=0;
        for(int i=0;i<4000;i++){
            String l = makeLit(i).s, c = makeCon(i).s, p = makePar(i,"P"+(i%3)).s;
            String[] ar = arr(i);
            if(i==3999){ System.out.println("field lit=["+l+"] con=["+c+"] par=["+p+"] arr0=["+ar[0]+"] arr1=["+ar[1]+"]"); }
            h += (l==null?1:l.length()) + (c==null?2:c.length()) + (ar[0]==null?4:ar[0].length());
        }
        System.out.println("h="+h);
        System.out.println("DONE");
    }
}
