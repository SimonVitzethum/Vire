public class Concat {
    public static void main(String[] args) {
        int x = 42;
        String name = "World";
        char c = '!';
        boolean b = true;

        System.out.println("Hello, " + name + c);
        System.out.println("x = " + x + ", doubled = " + (x * 2));
        System.out.println("bool: " + b);
        System.out.println(x + name);          // "42World" (int on left)
        System.out.println("empty:[" + "" + "]");

        // concatenation in a loop (many temporary strings)
        String acc = "";
        for (int i = 0; i < 5; i++) {
            acc = acc + i;
        }
        System.out.println(acc);               // "01234"
    }
}
