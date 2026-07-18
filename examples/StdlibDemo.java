import java.util.ArrayList;
import java.util.List;
import java.util.HashMap;
import java.util.Map;

public class StdlibDemo {
    public static void main(String[] args) {
        // real java.util.List with for-each
        List<String> names = new ArrayList<>();
        names.add("Anna");
        names.add("Bert");

        // real java.util.Map<String, Integer> with autoboxing
        Map<String, Integer> ages = new HashMap<>();
        ages.put("Anna", 30);
        ages.put("Bert", 25);
        ages.put("Cora", 40);

        for (String name : names) {
            System.out.println(name + " is " + ages.get(name) + " years");
        }

        System.out.println("has Cora: " + ages.containsKey("Cora"));  // true
        System.out.println("has Xaver: " + ages.containsKey("Xaver")); // false
        System.out.println("Map size: " + ages.size());               // 3

        // put returns old value
        Integer old = ages.put("Anna", 31);
        System.out.println("Anna before: " + old);       // 30
        System.out.println("Anna now: " + ages.get("Anna"));  // 31
    }
}
