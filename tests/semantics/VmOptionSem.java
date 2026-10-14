import com.sun.management.HotSpotDiagnosticMXBean;
import java.lang.management.ManagementFactory;

// VM options through HotSpotDiagnosticMXBean, as libraries read them to choose a code path
// (JOML asks for UseFMA). An option the VM does not have is an IllegalArgumentException.
public class VmOptionSem {
    public static void main(String[] args) {
        HotSpotDiagnosticMXBean bean = ManagementFactory.getPlatformMXBean(HotSpotDiagnosticMXBean.class);
        for (String name : new String[] {"UseFMA", "UseAVX", "UseSSE", "UseCompressedOops", "MaxHeapSize"}) {
            String v = bean.getVMOption(name).getValue();
            System.out.println(name + " present, " + (v.isEmpty() ? "empty" : "has a value"));
        }
        System.out.println("fma " + Math.fma(2.0, 3.0, 1.0));
        try {
            bean.getVMOption("NoSuchOptionAnywhere");
        } catch (IllegalArgumentException e) {
            System.out.println("unknown: " + e.getMessage());
        }
    }
}
