// Real hash map with open addressing (linear probing) — uses hashCode()
// to determine the bucket and equals() on collisions (both virtual
// dispatch via Object).
class MiniHashMap<K, V> {
    private Object[] keys;
    private Object[] vals;
    private int count;
    private int cap;

    MiniHashMap() {
        cap = 16;
        keys = new Object[cap];
        vals = new Object[cap];
        count = 0;
    }

    private int slotFor(Object k) {
        int h = k.hashCode() & 0x7fffffff;
        int i = h % cap;
        while (keys[i] != null && !keys[i].equals(k)) {
            i = (i + 1) % cap;
        }
        return i;
    }

    void put(K k, V v) {
        if (count * 2 >= cap) resize();
        int i = slotFor(k);
        if (keys[i] == null) count = count + 1;
        keys[i] = k;
        vals[i] = v;
    }

    V get(K k) {
        int i = slotFor(k);
        if (keys[i] == null) return null;
        return (V) vals[i];
    }

    boolean containsKey(K k) {
        return keys[slotFor(k)] != null;
    }

    int size() { return count; }

    private void resize() {
        Object[] oldK = keys;
        Object[] oldV = vals;
        cap = cap * 2;
        keys = new Object[cap];
        vals = new Object[cap];
        count = 0;
        for (int j = 0; j < oldK.length; j++) {
            if (oldK[j] != null) {
                int i = slotFor(oldK[j]);
                keys[i] = oldK[j];
                vals[i] = oldV[j];
                count = count + 1;
            }
        }
    }
}
