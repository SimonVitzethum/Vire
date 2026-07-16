public class HashMaps {
    public static void main(String[] args) {
        MiniHashMap<String, String> m = new MiniHashMap<>();
        m.put("rot", "FF0000");
        m.put("gruen", "00FF00");
        m.put("blau", "0000FF");

        System.out.println("rot: " + m.get("rot"));      // FF0000
        System.out.println("blau: " + m.get("blau"));    // 0000FF
        System.out.println("size: " + m.size());          // 3

        // konkatenierter Schlüssel (equals, nicht ==) trifft Bucket
        System.out.println("ro+t: " + m.get("ro" + "t")); // FF0000

        m.put("rot", "AA0000");                            // Update
        System.out.println("rot neu: " + m.get("rot"));   // AA0000
        System.out.println("size: " + m.size());           // 3

        System.out.println("hat gruen: " + m.containsKey("gruen"));  // true
        System.out.println("hat gelb: " + m.containsKey("gelb"));    // false
        System.out.println("gelb: " + (m.get("gelb") == null ? "null" : m.get("gelb")));

        // viele Einträge → Resize (Rehashing über hashCode)
        for (int i = 0; i < 50; i++) m.put("k" + i, "v" + i);
        System.out.println("size nach 50: " + m.size());   // 53
        System.out.println("k42: " + m.get("k42"));        // v42
        System.out.println("k7: " + m.get("k7"));          // v7
    }
}
