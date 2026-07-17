import java.util.Arrays;
public class ArraysDemo {
    public static void main(String[] args) {
        int[] a = new int[5];
        Arrays.fill(a, 7);
        System.out.println(Arrays.toString(a));   // [7, 7, 7, 7, 7]
        int[] b = {5, 2, 8, 1, 9, 3};
        Arrays.sort(b);
        System.out.println(Arrays.toString(b));    // [1, 2, 3, 5, 8, 9]
        int[] c = Arrays.copyOf(b, 3);
        System.out.println(Arrays.toString(c));    // [1, 2, 3]
    }
}
