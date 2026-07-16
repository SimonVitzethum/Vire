enum Color { RED, GREEN, BLUE }

public class Enum1 {
    public static void main(String[] args) {
        Color c = Color.GREEN;
        System.out.println(c.name());       // GREEN
        System.out.println(c.ordinal());    // 1
        for (Color x : Color.values()) System.out.println(x.name());
        Color d = Color.valueOf("BLUE");
        System.out.println(d.ordinal());    // 2
        System.out.println(d == Color.BLUE ? "same" : "diff"); // same
    }
}
