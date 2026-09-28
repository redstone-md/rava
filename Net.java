import io.netty.bootstrap.ServerBootstrap;
import io.netty.channel.*;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import io.netty.channel.socket.SocketChannel;
import java.net.InetSocketAddress;

// Mirrors net.minecraft.server.network.ServerConnectionListener.startTcpServerListener
public class Net {
    public static void main(String[] a) throws Exception {
        System.out.println("[net] creating group");
        EventLoopGroup group = new NioEventLoopGroup(0);
        System.out.println("[net] group=" + group);
        ChannelFuture f = new ServerBootstrap()
            .group(group)
            .channel(NioServerSocketChannel.class)
            .childHandler(new ChannelInitializer<SocketChannel>() {
                protected void initChannel(SocketChannel ch) {
                    System.out.println("[net] child " + ch);
                }
            })
            .localAddress(new InetSocketAddress("0.0.0.0", 25599))
            .bind()
            .syncUninterruptibly();
        System.out.println("[net] bound channel=" + f.channel());
        System.out.println("[net] DONE");
        group.shutdownGracefully();
    }
}
