import java.util.Set;
import java.util.HashSet;
import java.util.List;
import java.util.LinkedList;

public class CollDemo {
    public static void main(String[] args) {
        // HashSet with duplicate detection + for-each
        Set<String> s = new HashSet<>();
        s.add("red");
        s.add("green");
        s.add("red");   // duplicate
        System.out.println("set size = " + s.size());        // 2
        System.out.println("has green: " + s.contains("green")); // true
        System.out.println("has blue: " + s.contains("blue"));   // false
        int total = 0;
        for (String x : s) total = total + x.length();
        System.out.println("sum of lengths = " + total);       // 3+5 = 8

        // LinkedList with for-each and get
        List<String> l = new LinkedList<>();
        l.add("a");
        l.add("b");
        l.add("c");
        System.out.println("list size = " + l.size());        // 3
        System.out.println("list[1] = " + l.get(1));          // b
        for (String x : l) System.out.print(x);
        System.out.println();                                  // abc
    }
}
