public class Hello {
    public static void main(String[] a) {
        System.out.println("HELLO_RAVA");
        long acc = 0;
        for (int i = 0; i < 1000; i++) acc += i * 31L;
        System.out.println("CHECKSUM=" + acc);
        System.out.println("DONE");
    }
}
