#include <cstdio>
#include <vector>
using std::vector;
static void qsort_(vector<long>& a, long lo, long hi) {
    if (lo < hi) {
        long p = a[hi], i = lo - 1;
        for (long j = lo; j < hi; j++) if (a[j] < p) { i++; long t = a[i]; a[i] = a[j]; a[j] = t; }
        long t = a[i + 1]; a[i + 1] = a[hi]; a[hi] = t;
        qsort_(a, lo, i); qsort_(a, i + 2, hi);
    }
}
static long bsearch_(const vector<long>& a, long n, long key) {
    long lo = 0, hi = n - 1;
    while (lo <= hi) { long mid = (lo + hi) / 2; if (a[mid] == key) return mid; if (a[mid] < key) lo = mid + 1; else hi = mid - 1; }
    return -1;
}
int main() {
    long n = 200000; vector<long> a(n); long seed = 12345;
    for (long i = 0; i < n; i++) { seed = (seed * 1103515245 + 12345) % 2147483648L; a[i] = seed % 1000000; }
    qsort_(a, 0, n - 1);
    long hits = 0;
    for (long q = 0; q < 20000; q++) if (bsearch_(a, n, q * 50) >= 0) hits++;
    long hist[256] = {0};
    for (long i = 0; i < n; i++) hist[a[i] % 256]++;
    long checksum = 0;
    for (long k = 0; k < 256; k++) checksum = (checksum + hist[k] * (k + 1)) % 1000000007L;
    printf("%ld\n", hits * 1000000007L % 1000000007L + checksum * 1000 + hits);
}
