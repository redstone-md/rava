import java.net.*;
import java.nio.charset.StandardCharsets;

/** UDP over loopback: a DatagramSocket sends a packet another receives, with the sender's
 *  port, and the channel API's connected send and receive. */
public class DatagramSem {
    public static void main(String[] args) throws Exception {
        InetAddress lo = InetAddress.getByName("127.0.0.1");
        try (DatagramSocket a = new DatagramSocket(0, lo); DatagramSocket b = new DatagramSocket(0, lo)) {
            byte[] msg = "ping".getBytes(StandardCharsets.UTF_8);
            a.send(new DatagramPacket(msg, msg.length, lo, b.getLocalPort()));
            DatagramPacket p = new DatagramPacket(new byte[64], 64);
            b.setSoTimeout(5000);
            b.receive(p);
            System.out.println(new String(p.getData(), 0, p.getLength(), StandardCharsets.UTF_8));
            System.out.println(p.getPort() == a.getLocalPort());
            System.out.println(p.getAddress().getHostAddress());
        }
        try (var c = java.nio.channels.DatagramChannel.open(); var d = java.nio.channels.DatagramChannel.open()) {
            d.bind(new InetSocketAddress(lo, 0));
            c.connect(d.getLocalAddress());
            c.write(java.nio.ByteBuffer.wrap("pong".getBytes(StandardCharsets.UTF_8)));
            java.nio.ByteBuffer buf = java.nio.ByteBuffer.allocate(64);
            SocketAddress from = d.receive(buf);
            buf.flip();
            System.out.println(StandardCharsets.UTF_8.decode(buf) + " " + from.equals(c.getLocalAddress()));
        }
    }
}
