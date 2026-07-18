public class Maps {
    public static void main(String[] args) {
        MiniMap<String, String> caps = new MiniMap<>();
        caps.put("DE", "Berlin");
        caps.put("FR", "Paris");
        caps.put("IT", "Rom");

        System.out.println("DE: " + caps.get("DE"));   // Berlin
        System.out.println("IT: " + caps.get("IT"));   // Rom
        System.out.println("size: " + caps.size());    // 3

        // update key (equals matches the same String value)
        caps.put("DE", "Bonn");
        System.out.println("DE new: " + caps.get("DE"));  // Bonn
        System.out.println("size: " + caps.size());       // 3

        // missing key
        String x = caps.get("XX");
        System.out.println("XX: " + (x == null ? "missing" : x));

        // key as a freshly concatenated String (not the same object)
        String key = "D" + "E";
        System.out.println("concat-key DE: " + caps.get(key));  // Bonn (equals, not ==)

        // grow beyond the capacity
        for (int i = 0; i < 20; i++) caps.put("k" + i, "v" + i);
        System.out.println("size after grow: " + caps.size());  // 23
        System.out.println("k15: " + caps.get("k15"));         // v15
    }
}
