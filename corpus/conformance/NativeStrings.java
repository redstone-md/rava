import java.util.*;
public class NativeStrings {
    public static void main(String[] a){
        long h=0;
        for(int i=0;i<4000;i++){
            String lit="LIT", con="c"+(i%5), par=mk("p"+(i%5));
            // into collections (native)
            List<String> l=new ArrayList<>(); l.add(lit); l.add(con); l.add(par);
            Map<String,Integer> m=new HashMap<>(); m.put(con,i); m.put(lit,i);
            Set<String> s=new HashSet<>(); s.add(con); s.add("c"+(i%5));
            h += l.size()+m.size()+s.size();
            // string natives on concat result
            h += con.length() + con.hashCode() + (con.equals("c0")?1:0) + con.indexOf('c');
            h ^= String.valueOf(i).hashCode();
            // null string
            String ns = (i%7==0)? null : con;
            h += String.valueOf(ns).length();
            // exception message from concat across native boundary
            try{ throw new RuntimeException("E"+(i%3)); }catch(RuntimeException e){ h^=e.getMessage().hashCode(); }
        }
        System.out.println("h="+h);
        System.out.println("DONE");
    }
    static String mk(String s){ return s; }
}
