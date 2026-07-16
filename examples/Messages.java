// Exception-Messages: new RuntimeException(msg) und getMessage(), sowohl auf
// modellierten Exceptions als auch — null-tolerant — auf Laufzeit-Sentinels.
class Boom extends RuntimeException {
    Boom(String m) { super(m); }
}

public class Messages {
    public static void main(String[] args) {
        // 1. eingebaute RuntimeException mit Message
        try {
            throw new RuntimeException("plain");
        } catch (RuntimeException e) {
            System.out.println(e.getMessage());        // plain
        }

        // 2. benutzerdefinierte Exception, super(msg)
        try {
            throw new Boom("custom");
        } catch (RuntimeException e) {
            System.out.println(e.getMessage());        // custom
        }

        // 3. Message-loser Konstruktor → null
        try {
            throw new RuntimeException();
        } catch (RuntimeException e) {
            System.out.println(e.getMessage() == null ? "null-msg" : "?"); // null-msg
        }

        // 4. Laufzeit-Sentinel (ArithmeticException) via catch-all gefangen,
        //    getMessage() ist Sentinel-sicher → null
        try {
            int x = 1 / 0;
            System.out.println(x);
        } catch (RuntimeException e) {
            System.out.println(e.getMessage() == null ? "sentinel-null" : "?"); // sentinel-null
        }

        System.out.println("done");
    }
}
