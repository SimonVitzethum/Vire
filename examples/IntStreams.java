import java.util.stream.IntStream;

public class IntStreams {
    public static void main(String[] args) {
        // range + sum
        int s = IntStream.range(1, 6).sum();          // 1+2+3+4+5 = 15
        System.out.println("summe 1..5 = " + s);

        // range + map + sum
        int sq = IntStream.range(1, 6).map(x -> x * x).sum();  // 1+4+9+16+25 = 55
        System.out.println("summe quadrate = " + sq);

        // filter + count
        long even = IntStream.range(0, 20).filter(x -> x % 2 == 0).count();  // 10
        System.out.println("gerade 0..19 = " + even);

        // rangeClosed + forEach
        System.out.print("1..5:");
        IntStream.rangeClosed(1, 5).forEach(x -> System.out.print(" " + x));
        System.out.println();

        // Kette: filter + map + sum
        int r = IntStream.rangeClosed(1, 10).filter(x -> x % 2 == 1).map(x -> x * 10).sum();
        System.out.println("ungerade *10 = " + r);   // (1+3+5+7+9)*10 = 250
    }
}
