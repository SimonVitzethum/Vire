public class Format {
    public static void main(String[] args) {
        System.out.println(String.format("%d + %d = %d", 2, 3, 5));       // 2 + 3 = 5
        System.out.println(String.format("Name: %s, Alter: %d", "Anna", 30));
        System.out.println(String.format("Pi ~ %.2f", 3.14159));         // Pi ~ 3.14
        System.out.println(String.format("%5d|%-5d|", 42, 42));          // "   42|42   |"
        System.out.println(String.format("hex: %x, char: %c, bool: %b", 255, 65, true));
        System.out.printf("printf: %s = %d%n", "x", 7);
    }
}
