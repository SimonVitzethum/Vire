package java.util;

public interface Map<K, V> {
    V put(K key, V value);
    V get(Object key);
    int size();
    boolean containsKey(Object key);
}
