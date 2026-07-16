// Kleine Hash-Map über equals/hashCode der Schlüssel (virtueller Dispatch).
class MiniMap<K, V> {
    private Object[] keys;
    private Object[] vals;
    private int count;

    MiniMap() {
        keys = new Object[8];
        vals = new Object[8];
        count = 0;
    }

    void put(K k, V v) {
        for (int i = 0; i < count; i++) {
            if (keys[i].equals(k)) { vals[i] = v; return; }
        }
        if (count == keys.length) grow();
        keys[count] = k;
        vals[count] = v;
        count = count + 1;
    }

    V get(K k) {
        for (int i = 0; i < count; i++) {
            if (keys[i].equals(k)) return (V) vals[i];
        }
        return null;
    }

    int size() { return count; }

    private void grow() {
        Object[] nk = new Object[keys.length * 2];
        Object[] nv = new Object[vals.length * 2];
        for (int i = 0; i < count; i++) { nk[i] = keys[i]; nv[i] = vals[i]; }
        keys = nk;
        vals = nv;
    }
}
