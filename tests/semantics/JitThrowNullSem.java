public class JitThrowNullSem {
    static void raise(RuntimeException exception) { throw exception; }
    static void raiseNull() { throw (RuntimeException) null; }
    public static void main(String[] args) {
        RuntimeException original = new IllegalStateException("original");
        int nullCaught = 0, sameCaught = 0;
        String message = "";
        for (int i = 0; i < 4096; i++) {
            try { raise(original); }
            catch (RuntimeException e) { if (e == original) sameCaught++; }
            try { raiseNull(); }
            catch (NullPointerException e) { nullCaught++; message = e.getMessage(); }
        }
        System.out.println("null=" + nullCaught);
        System.out.println("same=" + sameCaught);
        System.out.println("message=" + message);
    }
}
