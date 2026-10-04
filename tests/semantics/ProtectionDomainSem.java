import java.net.URL;
import java.security.CodeSource;
import java.security.ProtectionDomain;
import java.security.cert.Certificate;

/** A class defined with a protection domain reports it (Class.getProtectionDomain), as code
 *  locating its own jar through getCodeSource().getLocation() expects. */
public class ProtectionDomainSem {
    public static class Payload {}

    static class Loader extends ClassLoader {
        Loader() { super(null); }
        Class<?> define(String name, byte[] b, ProtectionDomain pd) { return defineClass(name, b, 0, b.length, pd); }
    }

    public static void main(String[] args) throws Exception {
        byte[] b;
        try (var in = ProtectionDomainSem.class.getResourceAsStream("ProtectionDomainSem$Payload.class")) { b = in.readAllBytes(); }
        URL where = new URL("file:/opt/mods/payload.jar");
        ProtectionDomain pd = new ProtectionDomain(new CodeSource(where, (Certificate[]) null), null);
        Class<?> c = new Loader().define("ProtectionDomainSem$Payload", b, pd);
        System.out.println(c.getProtectionDomain() == pd);
        System.out.println(c.getProtectionDomain().getCodeSource().getLocation());
        Class<?> plain = new Loader().define("ProtectionDomainSem$Payload", b, null);
        System.out.println(plain.getProtectionDomain().getCodeSource());
    }
}
