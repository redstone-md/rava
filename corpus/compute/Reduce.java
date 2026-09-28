public class Reduce { public static void main(String[] a){
  int n=Integer.parseInt(a[0]); long acc=0;
  for(int i=0;i<n;i++){ acc += (long)i*2654435761L + (i^0x5bd1e995); }
  System.out.println(acc);
} }
