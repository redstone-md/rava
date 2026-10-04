// HotSpotDiagnosticMXBean.getVMOption: flags the VM has (value and origin), and a missing one.
import com.sun.management.HotSpotDiagnosticMXBean;
import java.lang.management.ManagementFactory;

public class FlagSem {
    public static void main(String[] args) {
        HotSpotDiagnosticMXBean b = ManagementFactory.getPlatformMXBean(HotSpotDiagnosticMXBean.class);
        for (String n : new String[] { "DisableExplicitGC", "UseCompressedOops", "NoSuchFlagXyz" }) {
            try {
                var o = b.getVMOption(n);
                System.out.println(n + " = " + o.getValue() + " " + o.getOrigin());
            } catch (IllegalArgumentException e) {
                System.out.println(n + " missing: " + e.getMessage());
            }
        }
    }
}
