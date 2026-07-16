import java.util.Set;
import java.util.HashSet;
import java.util.List;
import java.util.LinkedList;

public class CollDemo {
    public static void main(String[] args) {
        // HashSet mit Duplikat-Erkennung + for-each
        Set<String> s = new HashSet<>();
        s.add("rot");
        s.add("gruen");
        s.add("rot");   // Duplikat
        System.out.println("set size = " + s.size());        // 2
        System.out.println("hat gruen: " + s.contains("gruen")); // true
        System.out.println("hat blau: " + s.contains("blau"));   // false
        int total = 0;
        for (String x : s) total = total + x.length();
        System.out.println("summe laengen = " + total);       // 3+5 = 8

        // LinkedList mit for-each und get
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
