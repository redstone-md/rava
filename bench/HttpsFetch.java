// Times the EXACT JDK call authlib's MinecraftClient.get() makes for the
// yggdrasil services public key (the block-C "Failed to request yggdrasil
// public key" suspect): URL.openConnection -> HttpURLConnection with the same
// 15 s connect/read timeouts -> getResponseCode -> getInputStream.
//
// Purpose: decide between "the fetch stalls ~30 s under Rava" and "the fetch
// fails/succeeds fast and the block-C gap is elsewhere". Run under both:
//   rava.exe -cp <out> HttpsFetch
//   java     -cp <out> HttpsFetch
// No server port, no world — safe to run while other agents boot.
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;

public class HttpsFetch {
    static void attempt(String url) {
        long t0 = System.nanoTime();
        String outcome;
        try {
            HttpURLConnection c = (HttpURLConnection) new URL(url).openConnection();
            c.setConnectTimeout(15000);   // MinecraftClient.CONNECT_TIMEOUT_MS
            c.setReadTimeout(15000);      // MinecraftClient.READ_TIMEOUT_MS
            c.setRequestProperty("Content-Type", "application/json");
            int code = c.getResponseCode();
            int n = 0;
            try (InputStream in = c.getInputStream()) {
                byte[] buf = new byte[4096];
                for (int r; (r = in.read(buf)) > 0; ) n += r;
            }
            outcome = "code=" + code + " bytes=" + n;
        } catch (Exception e) {
            outcome = "threw " + e.getClass().getName() + ": " + e.getMessage();
        }
        long ms = (System.nanoTime() - t0) / 1_000_000L;
        System.out.println("FETCH url=" + url + " ms=" + ms + " " + outcome);
    }

    public static void main(String[] args) {
        // The real yggdrasil key URL authlib hits, then a second call to show
        // steady-state (class loading amortised).
        attempt("https://api.minecraftservices.com/publickeys");
        attempt("https://api.minecraftservices.com/publickeys");
        System.out.println("DONE");
    }
}
