public class SB {
    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        sb.append("Hallo").append(", ").append("Welt");
        sb.append('!').append(' ').append(42).append(' ').append(true);
        System.out.println(sb.toString());   // Hallo, Welt! 42 true
        System.out.println("len = " + sb.length());

        // StringBuilder(String) und Verkettung im Loop
        StringBuilder b = new StringBuilder("Zahlen:");
        for (int i = 0; i < 5; i++) b.append(' ').append(i);
        System.out.println(b.toString());    // Zahlen: 0 1 2 3 4
    }
}
