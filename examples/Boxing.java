public class Boxing {
    public static void main(String[] args) {
        // Autoboxing int -> Integer
        Integer a = 42;
        Integer b = 42;
        int back = a;                          // unboxing
        System.out.println("back = " + back);   // 42

        // equals on wrapper (value comparison, not identity)
        System.out.println(a.equals(b) ? 1 : 0);  // 1

        // Integer in concatenation (virtual toString)
        Integer n = 7;
        System.out.println("n = " + n);         // n = 7

        // Long and Boolean
        Long big = 10000000000L;
        System.out.println("big = " + big);     // 10000000000
        Boolean flag = true;
        System.out.println("flag = " + flag);   // flag = true

        // Integer as map value (get returns Integer, unboxing to int)
        MiniHashMap<String, Integer> scores = new MiniHashMap<>();
        scores.put("Anna", 95);
        scores.put("Bert", 88);
        int s = scores.get("Anna");            // unboxing
        System.out.println("Anna score = " + s);  // 95
        System.out.println("Bert: " + scores.get("Bert"));  // 88 (toString)

        // Integer as map KEY (hashCode/equals on Integer)
        MiniHashMap<Integer, String> byId = new MiniHashMap<>();
        byId.put(1, "one");
        byId.put(2, "two");
        byId.put(100, "hundred");
        System.out.println("id 2: " + byId.get(2));       // two
        System.out.println("id 100: " + byId.get(100));   // hundred
    }
}
