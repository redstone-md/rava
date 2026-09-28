public class CnpBench {
    // Pure-int static method: only cnp-supported opcodes (iload/istore/iinc,
    // imul/iadd/ixor/ishr, if_icmp loop). cnp compiles it; the whole inner loop
    // runs as inline native code per call.
    static int compute(int n) {
        int s = 1;
        for (int i = 0; i < n; i++) {
            s = s * 31 + i;
            s = s ^ (s >> 7);
        }
        return s;
    }
    public static void main(String[] args) {
        int r = 0;
        for (int k = 0; k < 30; k++) r += compute(1000000);
        System.out.println(r);
    }
}
