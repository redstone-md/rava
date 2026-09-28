import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import java.util.function.*;
public class Concurrency {
    interface S { int v(); }
    public static void main(String[] x) throws Exception {
        int NT=4; final int PER=20000;
        AtomicLong acc=new AtomicLong(0);
        CountDownLatch start=new CountDownLatch(1), done=new CountDownLatch(NT);
        S[] recv={ ()->1, ()->2, ()->3, ()->4, ()->5 };  // lambda receivers, poly
        for(int t=0;t<NT;t++){
            final int tid=t;
            new Thread(()->{
                try{ start.await(); }catch(InterruptedException e){}
                long local=0;
                for(int i=0;i<PER;i++){ S s=recv[(i+tid)%recv.length]; local+=s.v();
                    try{ if(i%1000==0) throw new RuntimeException("t"+tid); }catch(RuntimeException e){ local^=e.getMessage().hashCode(); } }
                acc.addAndGet(local); done.countDown();
            }).start();
        }
        start.countDown(); done.await();
        System.out.println("acc="+acc.get());
        System.out.println("DONE");
    }
}
