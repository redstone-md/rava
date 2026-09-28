public class Loop { public static void main(String[] a){
  int n = Integer.parseInt(a[0]); long h = 1125899906842597L;
  for (int i=1;i<=n;i++){ h = h*31 + (i%7); h ^= (i*2654435761L); }
  System.out.println(h);
} }
