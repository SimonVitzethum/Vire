package java.util;

// Minimales List-Interface (ohne die volle Collection/Iterable-Hierarchie
// des JDK; für fastjavac zählt nur die aufgerufene Methodensignatur).
public interface List<E> {
    boolean add(E e);
    E get(int i);
    int size();
    Iterator<E> iterator();
}
