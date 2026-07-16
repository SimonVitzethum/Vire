package java.util.stream;

import java.util.function.IntUnaryOperator;
import java.util.function.IntPredicate;
import java.util.function.IntConsumer;

public interface IntStream {
    static IntStream range(int start, int end) {
        int n = end - start;
        if (n < 0) n = 0;
        int[] a = new int[n];
        for (int i = 0; i < n; i++) a[i] = start + i;
        return new IntStreamImpl(a, n);
    }
    static IntStream rangeClosed(int start, int end) {
        return range(start, end + 1);
    }
    IntStream map(IntUnaryOperator f);
    IntStream filter(IntPredicate p);
    int sum();
    long count();
    void forEach(IntConsumer c);
}
