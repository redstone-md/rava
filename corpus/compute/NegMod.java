public class NegMod { public static void main(String[] a){ long h=0; for(int i=-50;i<=50;i++){ h=h*31+(i%7); h+=(i/3); } System.out.println(h);} }
