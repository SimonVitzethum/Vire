package java.util.stream;

import java.util.ArrayList;
import java.util.List;
import java.util.function.Function;
import java.util.function.Predicate;
import java.util.function.Consumer;
import java.util.function.BinaryOperator;

public class StreamImpl<T> implements Stream<T> {
    private ArrayList<T> data;

    public StreamImpl(ArrayList<T> d) {
        data = d;
    }

    public <R> Stream<R> map(Function<? super T, ? extends R> f) {
        ArrayList<R> out = new ArrayList<>();
        for (int i = 0; i < data.size(); i++) {
            out.add(f.apply(data.get(i)));
        }
        return new StreamImpl<R>(out);
    }

    public Stream<T> filter(Predicate<? super T> p) {
        ArrayList<T> out = new ArrayList<>();
        for (int i = 0; i < data.size(); i++) {
            T x = data.get(i);
            if (p.test(x)) out.add(x);
        }
        return new StreamImpl<T>(out);
    }

    public void forEach(Consumer<? super T> c) {
        for (int i = 0; i < data.size(); i++) {
            c.accept(data.get(i));
        }
    }

    public long count() {
        return data.size();
    }

    public List<T> toList() {
        ArrayList<T> out = new ArrayList<>();
        for (int i = 0; i < data.size(); i++) out.add(data.get(i));
        return out;
    }

    public T reduce(T identity, BinaryOperator<T> op) {
        T acc = identity;
        for (int i = 0; i < data.size(); i++) {
            acc = op.apply(acc, data.get(i));
        }
        return acc;
    }
}
