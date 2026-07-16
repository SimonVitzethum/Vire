public class Concat {
    public static void main(String[] args) {
        int x = 42;
        String name = "Welt";
        char c = '!';
        boolean b = true;

        System.out.println("Hallo, " + name + c);
        System.out.println("x = " + x + ", doppelt = " + (x * 2));
        System.out.println("bool: " + b);
        System.out.println(x + name);          // "42Welt" (int links)
        System.out.println("leer:[" + "" + "]");

        // Konkatenation in Schleife (viele temporäre Strings)
        String acc = "";
        for (int i = 0; i < 5; i++) {
            acc = acc + i;
        }
        System.out.println(acc);               // "01234"
    }
}
