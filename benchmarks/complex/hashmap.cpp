#include <cstdio>
#include <vector>
using std::vector;
static long lookup(const vector<long>& keys, const vector<long>& vals, long cap, long key) {
    long idx = (key * 2654435761L) % 2147483648L % cap, steps = 0;
    while (steps < cap) { long k = keys[idx]; if (k == 0) return 0; if (k == key) return vals[idx]; idx++; if (idx >= cap) idx = 0; steps++; }
    return 0;
}
static void insert(vector<long>& keys, vector<long>& vals, long cap, long key, long val) {
    long idx = (key * 2654435761L) % 2147483648L % cap;
    while (keys[idx] > 0) { idx++; if (idx >= cap) idx = 0; }
    keys[idx] = key; vals[idx] = val;
}
static void remove_(vector<long>& keys, long cap, long key) {
    long idx = (key * 2654435761L) % 2147483648L % cap, steps = 0;
    while (steps < cap) { long k = keys[idx]; if (k == 0) return; if (k == key) { keys[idx] = -1; return; } idx++; if (idx >= cap) idx = 0; steps++; }
}
int main() {
    long cap = 1048576; vector<long> keys(cap, 0), vals(cap, 0); long n = 400000;
    for (long i = 1; i <= n; i++) insert(keys, vals, cap, i, i * 3);
    long checksum = 0;
    for (long q = 1; q <= 800000; q++) { long key = q * 7919 % 800000 + 1; checksum = (checksum + lookup(keys, vals, cap, key)) % 1000000007L; }
    for (long i = 2; i <= n; i += 2) remove_(keys, cap, i);
    for (long q = 1; q <= 800000; q++) { long key = q * 7919 % 800000 + 1; checksum = (checksum + lookup(keys, vals, cap, key)) % 1000000007L; }
    printf("%ld\n", checksum);
}
