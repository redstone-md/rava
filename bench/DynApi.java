package probe;

public interface DynApi {
    default String label() {
        return "default-label";
    }

    void fail();
}
