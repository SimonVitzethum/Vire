package java.util.stream;

import java.util.function.IntUnaryOperator;
import java.util.function.IntPredicate;
import java.util.function.IntConsumer;

public class IntStreamImpl implements IntStream {
    private int[] data;
    private int len;

    public IntStreamImpl(int[] d, int n) {
        data = d;
        len = n;
    }

    public IntStream map(IntUnaryOperator f) {
        int[] out = new int[len];
        for (int i = 0; i < len; i++) out[i] = f.applyAsInt(data[i]);
        return new IntStreamImpl(out, len);
    }

    public IntStream filter(IntPredicate p) {
        int[] out = new int[len];
        int k = 0;
        for (int i = 0; i < len; i++) {
            if (p.test(data[i])) { out[k] = data[i]; k = k + 1; }
        }
        return new IntStreamImpl(out, k);
    }

    public int sum() {
        int s = 0;
        for (int i = 0; i < len; i++) s = s + data[i];
        return s;
    }

    public long count() { return len; }

    public void forEach(IntConsumer c) {
        for (int i = 0; i < len; i++) c.accept(data[i]);
    }
}
