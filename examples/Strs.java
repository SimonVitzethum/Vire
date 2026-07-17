public class Strs {
    public static void main(String[] args) {
        String s = "  Hello, World  ";
        String t = s.trim();
        System.out.println(t);                       // Hello, World
        System.out.println(t.substring(7));          // World
        System.out.println(t.substring(0, 5));       // Hello
        System.out.println(t.indexOf("World"));      // 7
        System.out.println(t.startsWith("Hello") ? "yes" : "no");  // yes
        System.out.println(t.endsWith("World") ? "yes" : "no");    // yes
        System.out.println("abc".concat("def"));     // abcdef
        System.out.println("apple".compareTo("banana") < 0 ? "lt" : "ge"); // lt
    }
}
