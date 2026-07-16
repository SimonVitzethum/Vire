package java.util;

public interface Set<E> {
    boolean add(E e);
    boolean contains(Object o);
    int size();
    Iterator<E> iterator();
}
