import java.util.ArrayList;
import java.util.List;

public class ForEach {
    public static void main(String[] args) {
        List<String> l = new ArrayList<>();
        l.add("Anna");
        l.add("Bert");
        l.add("Cora");

        // for-each über Iterator
        for (String s : l) {
            System.out.println("hallo " + s);
        }

        // klassisch mit Index
        System.out.println("size = " + l.size());
        System.out.println("erstes = " + l.get(0));
    }
}
