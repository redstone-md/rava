// Base64 conformance vs the JDK: basic/URL/MIME encoders (padding, wrapping,
// custom separators) + decoder round-trips, hashed into one SUM.
import java.util.Base64;
import java.nio.charset.StandardCharsets;

public class B64 {
    public static void main(String[] a) {
        long acc = 0;
        byte[][] inputs = {
            {}, {1}, {1, 2}, {1, 2, 3}, {(byte) 255, 0, (byte) 128, 7},
            "The quick brown fox jumps over the lazy dog".getBytes(StandardCharsets.UTF_8),
            new byte[300],
        };
        for (int i = 0; i < inputs[6].length; i++) inputs[6][i] = (byte) (i * 7);
        Base64.Encoder[] encs = {
            Base64.getEncoder(),
            Base64.getEncoder().withoutPadding(),
            Base64.getUrlEncoder(),
            Base64.getMimeEncoder(),
            Base64.getMimeEncoder(16, new byte[]{'\n'}),
        };
        for (byte[] in : inputs) {
            for (Base64.Encoder e : encs) {
                String s = e.encodeToString(in);
                for (int i = 0; i < s.length(); i++) acc = acc * 31 + s.charAt(i);
                byte[] back = (s.indexOf('\n') >= 0 || s.indexOf('\r') >= 0)
                    ? Base64.getMimeDecoder().decode(s)
                    : (s.indexOf('-') >= 0 || s.indexOf('_') >= 0)
                        ? Base64.getUrlDecoder().decode(s)
                        : Base64.getDecoder().decode(s);
                if (back.length != in.length) acc ^= 0xDEAD;
                for (byte b : back) acc = acc * 31 + b;
            }
        }
        System.out.println("SUM=" + acc);
    }
}
