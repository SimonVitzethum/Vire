public class Boxing2 {
    public static void main(String[] args) {
        Double d = 3.14;
        double dv = d;                          // unboxing
        System.out.println("d = " + d);         // 3.14
        System.out.println("dv*2 = " + (dv * 2)); // 6.28

        Character c = 'X';
        char cv = c;                            // unboxing
        System.out.println("c = " + c);         // X
        System.out.println("cv+1 = " + (cv + 1)); // 89 (int)

        Double d2 = 3.14;
        System.out.println(d.equals(d2) ? 1 : 0);  // 1

        // Character/Double als Map-Keys
        MiniHashMap<Character, String> m = new MiniHashMap<>();
        m.put('a', "Apfel");
        m.put('b', "Birne");
        System.out.println("a: " + m.get('a'));  // Apfel
        System.out.println("b: " + m.get('b'));  // Birne
    }
}
