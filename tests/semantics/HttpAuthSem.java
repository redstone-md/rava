import com.sun.net.httpserver.HttpServer;
import java.net.HttpURLConnection;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.URL;

// An HTTP 401 that offers NTLM: parsing its WWW-Authenticate header initializes the NTLM
// authentication class, whose Windows version asks a native whether trusted sites can be
// checked. Minecraft's authlib meets this on every offline start (the profile fetch answers 401).
public class HttpAuthSem {
    public static void main(String[] args) throws Exception {
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            ex.getResponseHeaders().add("WWW-Authenticate", "NTLM");
            ex.getResponseHeaders().add("WWW-Authenticate", "Basic realm=\"probe\"");
            ex.sendResponseHeaders(401, -1);
            ex.close();
        });
        server.start();
        try {
            URL url = URI.create("http://127.0.0.1:" + server.getAddress().getPort() + "/profile").toURL();
            HttpURLConnection c = (HttpURLConnection) url.openConnection();
            System.out.println("code " + c.getResponseCode());
            System.out.println("auth " + (c.getHeaderField("WWW-Authenticate") != null));
            c.disconnect();
        } finally {
            server.stop(0);
        }
        System.out.println("done");
    }
}
