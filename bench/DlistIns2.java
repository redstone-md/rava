// Step-bisect for the DoubleArrayList corruption: ctor-from-array vs add(0,v).
import it.unimi.dsi.fastutil.doubles.DoubleArrayList;

public class DlistIns2 {
    public static void main(String[] args) {
        DoubleArrayList l = new DoubleArrayList(new double[]{0.0, 1.0, 0.0, 0.0, 0.0});
        System.out.println("CTOR=" + dump(l));
        l.add(0, 1.5);
        System.out.println("INS=" + dump(l));
        DoubleArrayList e = new DoubleArrayList();
        e.add(7.0); e.add(8.0);
        System.out.println("APPEND=" + dump(e));
        e.add(1, 9.0);
        System.out.println("MIDINS=" + dump(e));
        DoubleArrayList w = DoubleArrayList.wrap(new double[]{3.0, 4.0, 5.0});
        System.out.println("WRAP=" + dump(w));
    }

    static String dump(DoubleArrayList l) {
        StringBuilder sb = new StringBuilder("size=" + l.size() + " [");
        for (int i = 0; i < l.size(); i++) sb.append(Double.doubleToLongBits(l.getDouble(i))).append(',');
        return sb.append(']').toString();
    }
}
