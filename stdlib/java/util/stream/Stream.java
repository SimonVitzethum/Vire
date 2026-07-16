package java.util.stream;

import java.util.function.Function;
import java.util.function.Predicate;
import java.util.function.Consumer;

public interface Stream<T> {
    <R> Stream<R> map(Function<? super T, ? extends R> f);
    Stream<T> filter(Predicate<? super T> p);
    void forEach(Consumer<? super T> c);
    long count();
}
