import jdk.jfr.*;

/** Flight Recorder's API without a recording: registering events, their types, committing. */
public class JfrSem {
    @Name("test.Sample")
    @Label("Sample")
    static class Sample extends Event {
        static final EventType TYPE = EventType.getEventType(Sample.class);
        @Label("value") int value;
    }

    @Name("test.Periodic")
    static class Periodic extends Event {
        int n;
    }

    public static void main(String[] args) {
        System.out.println("available " + FlightRecorder.isAvailable());
        FlightRecorder.register(Sample.class);
        FlightRecorder.register(Periodic.class);
        System.out.println("type " + Sample.TYPE.getName() + " " + Sample.TYPE.getLabel() + " " + Sample.TYPE.getFields().size());
        FlightRecorder.addPeriodicEvent(Periodic.class, () -> {});
        Sample s = new Sample();
        s.begin();
        s.value = 3;
        s.commit();
        System.out.println("enabled " + s.isEnabled() + " shouldCommit " + s.shouldCommit());
        System.out.println("registered " + EventType.getEventType(Periodic.class).getName());
    }
}
