import java.util.Arrays;

// The packages of the JDK's own classes: Class.getPackage and Package.getPackage(s) find
// them through the boot loader's package locations. Libraries read the specification version
// of java.lang to tell the Java release (cloth-config's TOML writer, for one).
public class BootPackageSem {
    public static void main(String[] args) {
        for (Class<?> c : new Class<?>[] {Runtime.class, java.util.List.class, java.util.concurrent.Future.class, java.sql.Date.class, BootPackageSem.class}) {
            Package p = c.getPackage();
            if (p == null) {
                System.out.println(c.getName() + ": no package");
                continue;
            }
            System.out.println(c.getName() + ": " + p.getName() + " spec=" + p.getSpecificationVersion() + " title=" + p.getSpecificationTitle()
                    + " vendor=" + p.getSpecificationVendor() + " impl=" + p.getImplementationVersion() + " sealed=" + p.isSealed());
        }
        System.out.println("primitive " + int.class.getPackage() + ", array " + int[].class.getPackage());
        Package[] all = Package.getPackages();
        System.out.println("java.lang listed " + Arrays.stream(all).anyMatch(p -> p.getName().equals("java.lang")));
        System.out.println("java.util listed " + Arrays.stream(all).anyMatch(p -> p.getName().equals("java.util")));
        System.out.println("unloaded package " + (ClassLoader.getPlatformClassLoader().getDefinedPackage("javax.print.attribute.standard") != null));
    }
}
