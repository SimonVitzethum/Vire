package java.util;

// Minimal List interface (without the full Collection/Iterable hierarchy
// of the JDK; for fastjavac only the invoked method signature matters).
public interface List<E> {
    boolean add(E e);
    E get(int i);
    int size();
    Iterator<E> iterator();
}
