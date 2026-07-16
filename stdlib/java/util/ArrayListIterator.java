package java.util;

class ArrayListIterator<E> implements Iterator<E> {
    private ArrayList<E> list;
    private int pos;

    ArrayListIterator(ArrayList<E> l) {
        list = l;
        pos = 0;
    }

    public boolean hasNext() {
        return pos < list.size();
    }

    public E next() {
        E e = list.get(pos);
        pos = pos + 1;
        return e;
    }
}
