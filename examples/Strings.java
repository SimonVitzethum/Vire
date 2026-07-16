public class Strings {
    public static void main(String[] args) {
        String s = "Hello";
        System.out.print("length = ");
        System.out.println(s.length());        // 5

        System.out.print("chars: ");
        for (int i = 0; i < s.length(); i++) {
            System.out.print(s.charAt(i));      // Hello
        }
        System.out.println();

        String a = "abc";
        String b = "abc";
        String c = "xyz";
        System.out.println(a.equals(b) ? 1 : 0);   // 1
        System.out.println(a.equals(c) ? 1 : 0);   // 0

        System.out.println("".isEmpty() ? 1 : 0);  // 1
        System.out.println(s.isEmpty() ? 1 : 0);   // 0

        // Zeichen zählen die 'l' sind
        int count = 0;
        for (int i = 0; i < s.length(); i++) {
            if (s.charAt(i) == 'l') count++;
        }
        System.out.print("l-count = ");
        System.out.println(count);              // 2
    }
}
