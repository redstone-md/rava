// Multicast as LAN game discovery uses it: join a group on a MulticastSocket, the outgoing
// interface, a source-specific join on a DatagramChannel, leaving. (Whether a send to the group
// gets out depends on the host's routes, so none is made.)
import java.net.*; import java.nio.channels.*; import java.nio.*;
public class MulticastSem { public static void main(String[] a) throws Exception {
  InetAddress group = InetAddress.getByName("224.0.2.60");
  try (MulticastSocket s = new MulticastSocket(0)) {
    try { s.joinGroup(group); System.out.println("joined"); } catch (Exception e) { System.out.println("join " + e.getClass().getName()); }
    System.out.println("if " + s.getInterface());
    try { s.setInterface(InetAddress.getByName("127.0.0.1")); System.out.println("if " + s.getInterface()); } catch (Exception e) { System.out.println("setIf " + e.getClass().getName()); }
    s.setTimeToLive(1); System.out.println("ttl " + s.getTimeToLive());
    try { s.leaveGroup(group); System.out.println("left"); } catch (Exception e) { System.out.println("leave " + e.getClass().getName()); }
  }
  try (DatagramChannel ch = DatagramChannel.open(StandardProtocolFamily.INET)) {
    NetworkInterface lo = NetworkInterface.getByInetAddress(InetAddress.getByName("127.0.0.1"));
    ch.setOption(StandardSocketOptions.IP_MULTICAST_IF, lo);
    System.out.println("chIf " + (ch.getOption(StandardSocketOptions.IP_MULTICAST_IF) != null));
    try { MembershipKey k = ch.join(InetAddress.getByName("232.1.1.1"), lo, InetAddress.getByName("127.0.0.1")); System.out.println("ssm " + k.isValid()); k.drop(); System.out.println("dropped " + k.isValid()); }
    catch (Exception e) { System.out.println("ssm " + e.getClass().getName()); }
  }
} }
