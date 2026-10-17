// ThreadMXBean as profilers use it: ThreadInfo of threads sleeping, waiting, parked and blocked
// on a monitor (state, lock, the innermost frames), and dumpAllThreads, also with locked monitors.
import java.lang.management.*; import java.util.*; import java.util.concurrent.locks.LockSupport;
public class ThreadMxSem {
  static final Object LOCK = new Object(), HELD = new Object();
  static void spin(Thread t, Thread.State s) throws Exception { while (t.getState() != s) Thread.sleep(5); Thread.sleep(50); }
  public static void main(String[] a) throws Exception {
    ThreadMXBean mx = ManagementFactory.getThreadMXBean();
    Thread sleeper = new Thread(() -> { try { Thread.sleep(60_000); } catch (InterruptedException e) {} }, "sleeper");
    Thread waiter = new Thread(() -> { synchronized (LOCK) { try { LOCK.wait(); } catch (InterruptedException e) {} } }, "waiter");
    Thread parker = new Thread(() -> LockSupport.park(), "parker");
    Thread blocked = new Thread(() -> { synchronized (HELD) {} }, "blocked");
    for (Thread t : List.of(sleeper, waiter, parker)) { t.setDaemon(true); t.start(); }
    blocked.setDaemon(true);
    spin(sleeper, Thread.State.TIMED_WAITING); spin(waiter, Thread.State.WAITING); spin(parker, Thread.State.WAITING);
    synchronized (HELD) {
      blocked.start(); spin(blocked, Thread.State.BLOCKED);
      for (Thread t : List.of(sleeper, waiter, parker, blocked)) {
        ThreadInfo i = mx.getThreadInfo(t.threadId(), 3);
        StringBuilder sb = new StringBuilder(i.getThreadName() + " " + i.getThreadState() + " lock=" + (i.getLockName() != null && i.getLockName().startsWith("java.lang.Object")));
        for (StackTraceElement e : i.getStackTrace()) sb.append(" | ").append(e.getClassName().replaceAll("/0x[0-9a-f]+", "")).append('.').append(e.getMethodName());
        System.out.println(sb);
      }
      ThreadInfo[] all = mx.dumpAllThreads(false, false);
      System.out.println("dump has ours " + Arrays.stream(all).map(ThreadInfo::getThreadName).filter(n -> List.of("sleeper", "waiter", "parker", "blocked", "main").contains(n)).sorted().toList());
      System.out.println("ids " + (mx.getAllThreadIds().length >= 5) + " missing " + (mx.getThreadInfo(Long.MAX_VALUE) == null));
      // A dump with locked monitors and synchronizers (Minecraft's /perf report takes one).
      System.out.println("monitor usage " + mx.isObjectMonitorUsageSupported() + " synchronizer usage " + mx.isSynchronizerUsageSupported());
      ThreadInfo[] locked = mx.dumpAllThreads(true, true);
      System.out.println("locked dump has ours " + Arrays.stream(locked).map(ThreadInfo::getThreadName).filter(n -> List.of("sleeper", "waiter", "parker", "blocked", "main").contains(n)).sorted().toList());
    }
  }
}
