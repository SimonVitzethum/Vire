public class SB {
    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        sb.append("Hello").append(", ").append("World");
        sb.append('!').append(' ').append(42).append(' ').append(true);
        System.out.println(sb.toString());   // Hello, World! 42 true
        System.out.println("len = " + sb.length());

        // StringBuilder(String) and concatenation in a loop
        StringBuilder b = new StringBuilder("Numbers:");
        for (int i = 0; i < 5; i++) b.append(' ').append(i);
        System.out.println(b.toString());    // Numbers: 0 1 2 3 4
    }
}
