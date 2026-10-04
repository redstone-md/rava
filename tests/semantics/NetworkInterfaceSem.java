// NetworkInterface: the host's interfaces with their addresses, flags, MTU and hardware address.
import java.net.*; import java.util.*;
public class NetworkInterfaceSem { public static void main(String[] a) throws Exception {
  for (NetworkInterface n : Collections.list(NetworkInterface.getNetworkInterfaces())) {
    byte[] mac = n.getHardwareAddress();
    System.out.println(n.getName() + " idx=" + n.getIndex() + " up=" + n.isUp() + " lo=" + n.isLoopback() + " mc=" + n.supportsMulticast() + " mtu=" + n.getMTU() + " mac=" + (mac == null ? "null" : mac.length) + " " + n.getInterfaceAddresses());
  }
  System.out.println(NetworkInterface.getByName("lo").getName() + " " + NetworkInterface.getByInetAddress(InetAddress.getByName("127.0.0.1")).getName() + " " + NetworkInterface.getByIndex(1).getName());
} }
