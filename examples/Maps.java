public class Maps {
    public static void main(String[] args) {
        MiniMap<String, String> caps = new MiniMap<>();
        caps.put("DE", "Berlin");
        caps.put("FR", "Paris");
        caps.put("IT", "Rom");

        System.out.println("DE: " + caps.get("DE"));   // Berlin
        System.out.println("IT: " + caps.get("IT"));   // Rom
        System.out.println("size: " + caps.size());    // 3

        // Schlüssel aktualisieren (equals matcht denselben String-Wert)
        caps.put("DE", "Bonn");
        System.out.println("DE neu: " + caps.get("DE"));  // Bonn
        System.out.println("size: " + caps.size());       // 3

        // fehlender Schlüssel
        String x = caps.get("XX");
        System.out.println("XX: " + (x == null ? "fehlt" : x));

        // Schlüssel als frisch konkatenierter String (nicht dasselbe Objekt)
        String key = "D" + "E";
        System.out.println("konkat-key DE: " + caps.get(key));  // Bonn (equals, nicht ==)

        // über die Kapazität hinaus wachsen
        for (int i = 0; i < 20; i++) caps.put("k" + i, "v" + i);
        System.out.println("size nach grow: " + caps.size());  // 23
        System.out.println("k15: " + caps.get("k15"));         // v15
    }
}
