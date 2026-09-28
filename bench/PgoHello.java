// Smallest possible guest run, used only to find out whether running ANY guest
// program breaks the PGO profile writer (a full driver leaves a 0-byte .profraw
// even though `rava --help` writes 5.2 MB).
public class PgoHello {
    public static void main(String[] args) {
        long s = 0;
        for (int i = 0; i < 1_000_00; i++) s += i;
        System.out.println("PGOHELLO=" + s);
    }
}
