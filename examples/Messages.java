// Exception messages: new RuntimeException(msg) and getMessage(), both on
// modeled exceptions and — null-tolerant — on runtime sentinels.
class Boom extends RuntimeException {
    Boom(String m) { super(m); }
}

public class Messages {
    public static void main(String[] args) {
        // 1. built-in RuntimeException with message
        try {
            throw new RuntimeException("plain");
        } catch (RuntimeException e) {
            System.out.println(e.getMessage());        // plain
        }

        // 2. user-defined exception, super(msg)
        try {
            throw new Boom("custom");
        } catch (RuntimeException e) {
            System.out.println(e.getMessage());        // custom
        }

        // 3. message-less constructor → null
        try {
            throw new RuntimeException();
        } catch (RuntimeException e) {
            System.out.println(e.getMessage() == null ? "null-msg" : "?"); // null-msg
        }

        // 4. runtime sentinel (ArithmeticException) caught via catch-all,
        //    getMessage() is sentinel-safe → null
        try {
            int x = 1 / 0;
            System.out.println(x);
        } catch (RuntimeException e) {
            System.out.println(e.getMessage() == null ? "sentinel-null" : "?"); // sentinel-null
        }

        System.out.println("done");
    }
}
