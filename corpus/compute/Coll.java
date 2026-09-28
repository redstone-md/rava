import java.util.*;
public class Coll { public static void main(String[] a){ Map<String,Integer> m=new HashMap<>(); for(int i=0;i<500;i++) m.merge("k"+(i%50),i,Integer::sum); System.out.println(m.size()); } }
