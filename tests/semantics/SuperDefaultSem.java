// A super call (invokespecial) naming a superclass that inherits the method only as an
// interface default: the maximally specific default must run, the one a subinterface
// redeclares, not the one it overrides. Minecraft's scrolling lists forward drags to their
// rows this way (AbstractSelectionList.mouseDragged -> super -> ContainerEventHandler's
// default -> the focused child).
public class SuperDefaultSem {
    interface Listener {
        default String drag(int x) { return "Listener.drag"; }
        default String scroll(int x) { return "Listener.scroll"; }
        default String click(int x) { return "Listener.click"; }
    }

    interface Container extends Listener {
        @Override
        default String drag(int x) { return "Container.drag " + x; }
        @Override
        default String scroll(int x) { return "Container.scroll " + x; }
    }

    interface Inner extends Container {
        @Override
        default String scroll(int x) { return "Inner.scroll " + x; }
    }

    static abstract class Handler implements Container {}

    static abstract class Deeper extends Handler implements Inner {}

    static class List extends Handler {
        @Override
        public String drag(int x) { return "List(" + super.drag(x) + ")"; }
        @Override
        public String scroll(int x) { return "List(" + super.scroll(x) + ")"; }
        @Override
        public String click(int x) { return "List(" + super.click(x) + ")"; }
    }

    static class DeepList extends Deeper {
        @Override
        public String drag(int x) { return "Deep(" + super.drag(x) + ")"; }
        @Override
        public String scroll(int x) { return "Deep(" + super.scroll(x) + ")"; }
    }

    static class Plain extends Handler {}

    static class DeepPlain extends Deeper {}

    public static void main(String[] args) {
        List l = new List();
        DeepList d = new DeepList();
        Plain p = new Plain();
        Listener dp = new DeepPlain();
        Deeper dq = new DeepPlain();
        for (int i = 0; i < 3; i++) {
            System.out.println(l.drag(i) + " | " + l.scroll(i) + " | " + l.click(i));
            System.out.println(d.drag(i) + " | " + d.scroll(i));
            System.out.println(p.drag(i) + " | " + p.scroll(i));
            System.out.println(dp.scroll(i) + " | " + dq.scroll(i) + " | " + ((Container) dq).scroll(i) + " | " + dq.click(i));
        }
        // Hot, for the compilers.
        int n = 0;
        for (int i = 0; i < 200000; i++) {
            n += l.drag(i & 7).length() + d.scroll(i & 7).length();
        }
        System.out.println("hot " + n);
    }
}
