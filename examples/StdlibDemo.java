import java.util.ArrayList;
import java.util.List;
import java.util.HashMap;
import java.util.Map;

public class StdlibDemo {
    public static void main(String[] args) {
        // echte java.util.List mit for-each
        List<String> names = new ArrayList<>();
        names.add("Anna");
        names.add("Bert");

        // echte java.util.Map<String, Integer> mit Autoboxing
        Map<String, Integer> ages = new HashMap<>();
        ages.put("Anna", 30);
        ages.put("Bert", 25);
        ages.put("Cora", 40);

        for (String name : names) {
            System.out.println(name + " ist " + ages.get(name) + " Jahre");
        }

        System.out.println("hat Cora: " + ages.containsKey("Cora"));  // true
        System.out.println("hat Xaver: " + ages.containsKey("Xaver")); // false
        System.out.println("Map-Größe: " + ages.size());               // 3

        // put gibt alten Wert
        Integer old = ages.put("Anna", 31);
        System.out.println("Anna vorher: " + old);       // 30
        System.out.println("Anna jetzt: " + ages.get("Anna"));  // 31
    }
}
