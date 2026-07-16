package java.util;

class HashSetIterator<E> implements Iterator<E> {
    private HashSet<E> set;
    private int pos;

    HashSetIterator(HashSet<E> s) {
        set = s;
        pos = 0;
        advance();
    }

    private void advance() {
        while (pos < set.slots.length && set.slots[pos] == null) {
            pos = pos + 1;
        }
    }

    public boolean hasNext() {
        return pos < set.slots.length;
    }

    public E next() {
        E e = (E) set.slots[pos];
        pos = pos + 1;
        advance();
        return e;
    }
}
