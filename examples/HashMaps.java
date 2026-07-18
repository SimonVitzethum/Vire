public class HashMaps {
    public static void main(String[] args) {
        MiniHashMap<String, String> m = new MiniHashMap<>();
        m.put("red", "FF0000");
        m.put("green", "00FF00");
        m.put("blue", "0000FF");

        System.out.println("red: " + m.get("red"));      // FF0000
        System.out.println("blue: " + m.get("blue"));    // 0000FF
        System.out.println("size: " + m.size());          // 3

        // concatenated key (equals, not ==) hits bucket
        System.out.println("re+d: " + m.get("re" + "d")); // FF0000

        m.put("red", "AA0000");                            // update
        System.out.println("red new: " + m.get("red"));   // AA0000
        System.out.println("size: " + m.size());           // 3

        System.out.println("has green: " + m.containsKey("green"));  // true
        System.out.println("has gold: " + m.containsKey("gold"));    // false
        System.out.println("gold: " + (m.get("gold") == null ? "null" : m.get("gold")));

        // many entries → resize (rehashing via hashCode)
        for (int i = 0; i < 50; i++) m.put("k" + i, "v" + i);
        System.out.println("size after 50: " + m.size());   // 53
        System.out.println("k42: " + m.get("k42"));        // v42
        System.out.println("k7: " + m.get("k7"));          // v7
    }
}
