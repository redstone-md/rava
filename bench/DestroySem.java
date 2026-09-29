public class DestroySem {
    public static void main(String[] args) {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> System.out.println("hook")));
        Thread d = new Thread(() -> { try { Thread.sleep(100000); } catch (InterruptedException e) {} });
        d.setDaemon(true);
        d.start();
        new Thread(() -> {
            try { Thread.sleep(300); } catch (InterruptedException e) {}
            System.out.println("worker done");
        }, "worker").start();
        System.out.println("main done");
    }
}
