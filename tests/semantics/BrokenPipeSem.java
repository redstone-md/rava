// Writing to a socket whose peer has closed fails with an IOException after at most a few
// writes (EPIPE / ECONNRESET), instead of a write reported as zero bytes and retried forever.
import java.io.IOException;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;

public class BrokenPipeSem {
    public static void main(String[] args) throws Exception {
        try (ServerSocket ss = new ServerSocket(0, 1, InetAddress.getLoopbackAddress())) {
            Socket c = new Socket(InetAddress.getLoopbackAddress(), ss.getLocalPort());
            ss.accept().close();
            OutputStream out = c.getOutputStream();
            byte[] chunk = new byte[64 << 10];
            int writes = 0;
            try {
                while (writes < 10000) {
                    out.write(chunk);
                    out.flush();
                    writes++;
                    Thread.sleep(2);
                }
                System.out.println("no failure");
            } catch (IOException e) {
                System.out.println("failed with " + (e instanceof java.net.SocketException ? "SocketException" : "IOException") + " after few writes: " + (writes < 100));
            }
            c.close();
        }
    }
}
