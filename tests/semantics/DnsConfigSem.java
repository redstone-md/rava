import java.util.Hashtable;
import javax.naming.Context;
import javax.naming.directory.DirContext;
import javax.naming.directory.InitialDirContext;

// A JNDI DNS context without a provider URL asks the platform for its name servers and search
// list (sun.net.dns.ResolverConfiguration). Minecraft's SRV redirect resolver creates one for
// the server list; on Windows the configuration comes from natives.
public class DnsConfigSem {
    public static void main(String[] args) throws Exception {
        Hashtable<String, String> env = new Hashtable<>();
        env.put(Context.INITIAL_CONTEXT_FACTORY, "com.sun.jndi.dns.DnsContextFactory");
        env.put("com.sun.jndi.dns.timeout.initial", "100");
        env.put("com.sun.jndi.dns.timeout.retries", "1");
        DirContext ctx = new InitialDirContext(env);
        Object url = ctx.getEnvironment().get(Context.PROVIDER_URL);
        System.out.println("context " + ctx.getClass().getSimpleName());
        System.out.println("provider url " + url);
        ctx.close();
    }
}
