import java.lang.reflect.*;
public class Reflect {
    static class T { int base=10; int add(int x){return base+x;} static int smul(int x){return x*4;} }
    public static void main(String[] a) throws Exception {
        long h=0; Class<?> c = T.class;
        Method add = c.getDeclaredMethod("add", int.class);
        Method smul = c.getDeclaredMethod("smul", int.class);
        Constructor<?> ctor = c.getDeclaredConstructor();
        for(int i=0;i<3000;i++){
            Object o = ctor.newInstance();
            h += (Integer)add.invoke(o, i%10);
            h += (Integer)smul.invoke(null, i%10);
        }
        System.out.println("refl="+h);
        // InvocationTargetException cause/message
        try { Method bad = c.getDeclaredMethod("add", int.class); bad.invoke(null, 1); }
        catch(Exception e){ System.out.println("expectedThrow="+e.getClass().getSimpleName()); }
        System.out.println("DONE");
    }
}
