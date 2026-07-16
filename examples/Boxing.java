public class Boxing {
    public static void main(String[] args) {
        // Autoboxing int -> Integer
        Integer a = 42;
        Integer b = 42;
        int back = a;                          // Unboxing
        System.out.println("back = " + back);   // 42

        // equals auf Wrapper (Wert-Vergleich, nicht Identität)
        System.out.println(a.equals(b) ? 1 : 0);  // 1

        // Integer in Konkatenation (virtueller toString)
        Integer n = 7;
        System.out.println("n = " + n);         // n = 7

        // Long und Boolean
        Long big = 10000000000L;
        System.out.println("big = " + big);     // 10000000000
        Boolean flag = true;
        System.out.println("flag = " + flag);   // flag = true

        // Integer als Map-Value (get gibt Integer, unboxing zu int)
        MiniHashMap<String, Integer> scores = new MiniHashMap<>();
        scores.put("Anna", 95);
        scores.put("Bert", 88);
        int s = scores.get("Anna");            // unboxing
        System.out.println("Anna score = " + s);  // 95
        System.out.println("Bert: " + scores.get("Bert"));  // 88 (toString)

        // Integer als Map-KEY (hashCode/equals auf Integer)
        MiniHashMap<Integer, String> byId = new MiniHashMap<>();
        byId.put(1, "eins");
        byId.put(2, "zwei");
        byId.put(100, "hundert");
        System.out.println("id 2: " + byId.get(2));       // zwei
        System.out.println("id 100: " + byId.get(100));   // hundert
    }
}
