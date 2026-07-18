import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;

public class Streams2 {
    public static void main(String[] args) {
        ArrayList<Integer> nums = new ArrayList<>();
        for (int i = 1; i <= 5; i++) nums.add(i);

        // reduce: sum
        Integer sum = nums.stream().reduce(0, (a, b) -> a + b);
        System.out.println("sum = " + sum);          // 15

        // reduce: product
        Integer prod = nums.stream().reduce(1, (a, b) -> a * b);
        System.out.println("product = " + prod);        // 120

        // filter + map + reduce
        Integer s2 = nums.stream()
            .filter(x -> x % 2 == 0)
            .map(x -> x * x)
            .reduce(0, (a, b) -> a + b);
        System.out.println("sum of even squares = " + s2);  // 4+16=20

        // toList after map
        List<Integer> doubled = nums.stream().map(x -> x * 2).toList();
        System.out.println("doubled size = " + doubled.size());  // 5
        System.out.println("first = " + doubled.get(0));            // 2
        System.out.println("last = " + doubled.get(4));           // 10
    }
}
