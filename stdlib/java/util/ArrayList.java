package java.util;

public class ArrayList<E> implements List<E> {
    private Object[] elems;
    private int count;

    public ArrayList() {
        elems = new Object[4];
        count = 0;
    }

    public boolean add(E e) {
        if (count == elems.length) grow();
        elems[count] = e;
        count = count + 1;
        return true;
    }

    public E get(int i) {
        return (E) elems[i];
    }

    public int size() {
        return count;
    }

    public Iterator<E> iterator() {
        return new ArrayListIterator<E>(this);
    }

    public java.util.stream.Stream<E> stream() {
        return new java.util.stream.StreamImpl<E>(this);
    }

    private void grow() {
        Object[] bigger = new Object[elems.length * 2];
        for (int i = 0; i < count; i++) bigger[i] = elems[i];
        elems = bigger;
    }
}
