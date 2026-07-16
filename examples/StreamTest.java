import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;
public class StreamTest {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("aa"); l.add("b"); l.add("ccc");
        l.stream().filter(s -> s.length() >= 2).forEach(s -> System.out.println(s));
    }
}
