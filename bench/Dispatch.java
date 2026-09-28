// Generic polymorphic dispatch — invokevirtual / invokeinterface hot path.
// Stresses the call-site cache and (Phase C) inlining of small virtuals.
public class Dispatch {
    interface Shape { double area(); }
    static final class Circle implements Shape { double r; Circle(double r){this.r=r;} public double area(){return 3.14159*r*r;} }
    static final class Square implements Shape { double s; Square(double s){this.s=s;} public double area(){return s*s;} }
    static final class Tri    implements Shape { double b,h; Tri(double b,double h){this.b=b;this.h=h;} public double area(){return 0.5*b*h;} }
    static double hot(int iters, Shape[] shapes) {
        double acc = 0;
        for (int i = 0; i < iters; i++) acc += shapes[i % shapes.length].area();
        return acc;
    }
    public static void main(String[] a) {
        Shape[] shapes = { new Circle(2), new Square(3), new Tri(4,5), new Circle(1), new Square(2) };
        for (int w = 0; w < 15; w++) hot(1_000_000, shapes);  // warmup
        long t0 = System.nanoTime();
        double s = hot(20_000_000, shapes);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + (long) s);
        System.out.println("HOTNS=" + ns);
    }
}
