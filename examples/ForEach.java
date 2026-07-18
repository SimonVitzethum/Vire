import java.util.ArrayList;
import java.util.List;

public class ForEach {
    public static void main(String[] args) {
        List<String> l = new ArrayList<>();
        l.add("Anna");
        l.add("Bert");
        l.add("Cora");

        // for-each over iterator
        for (String s : l) {
            System.out.println("hello " + s);
        }

        // classic with index
        System.out.println("size = " + l.size());
        System.out.println("first = " + l.get(0));
    }
}
