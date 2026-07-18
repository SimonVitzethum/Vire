public class StrNpe {
    public static void main(String[] args) {
        String s = null;
        try {
            int n = s.length();
            System.out.println("not reached " + n);
        } catch (NullPointerException e) {
            System.out.println("str-length-npe caught");
        }
        String t = "abc";
        try {
            char c = t.charAt(10);
            System.out.println("not reached " + c);
        } catch (StringIndexOutOfBoundsException e) {
            System.out.println("charAt-bounds caught");
        }
        System.out.println("len of abc: " + t.length());  // 3
        System.out.println("continue");
    }
}
