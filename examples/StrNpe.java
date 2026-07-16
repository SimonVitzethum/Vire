public class StrNpe {
    public static void main(String[] args) {
        String s = null;
        try {
            int n = s.length();
            System.out.println("nicht erreicht " + n);
        } catch (NullPointerException e) {
            System.out.println("str-length-npe gefangen");
        }
        String t = "abc";
        try {
            char c = t.charAt(10);
            System.out.println("nicht erreicht " + c);
        } catch (StringIndexOutOfBoundsException e) {
            System.out.println("charAt-bounds gefangen");
        }
        System.out.println("len von abc: " + t.length());  // 3
        System.out.println("weiter");
    }
}
