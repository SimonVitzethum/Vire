import java.util.ArrayList;
public class St3 {
    public static void main(String[] args) {
        ArrayList<Integer> l = new ArrayList<>();
        l.add(1); l.add(2);
        l.stream().map(x -> x + 10).forEach(n -> System.out.println(n));
    }
}
