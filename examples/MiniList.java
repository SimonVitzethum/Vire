// Generic list — javac erases the type parameters to Object; the
// compiler sees Object[]/Object signatures, and the caller gets
// checkcast inserted automatically.
class MiniList<E> {
    private Object[] elems;
    private int count;

    MiniList() {
        elems = new Object[4];
        count = 0;
    }

    void add(E e) {
        if (count == elems.length) grow();
        elems[count] = e;
        count = count + 1;
    }

    E get(int i) {
        return (E) elems[i];
    }

    void set(int i, E e) {
        elems[i] = e;
    }

    int size() {
        return count;
    }

    private void grow() {
        Object[] bigger = new Object[elems.length * 2];
        for (int i = 0; i < count; i++) bigger[i] = elems[i];
        elems = bigger;
    }
}
