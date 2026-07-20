#include <cstdio>
#include <vector>
#include <thread>
#include <atomic>
using std::vector;
static void qsort_(vector<long>& a, long lo, long hi) {
    if (lo < hi) {
        long p = a[(lo + hi) / 2], i = lo, j = hi;
        while (i <= j) {
            while (a[i] < p) i++;
            while (a[j] > p) j--;
            if (i <= j) { long t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
        }
        qsort_(a, lo, j);
        qsort_(a, i, hi);
    }
}
static long worker(long base) {
    long n = 1000000; vector<long> a(n); long seed = base * 2654435761L + 12345;
    for (long i = 0; i < n; i++) { seed = (seed * 1103515245 + 12345) % 2147483648L; a[i] = seed % 1000000; }
    qsort_(a, 0, n - 1);
    long cs = 0;
    for (long i = 0; i < n; i++) cs = (cs + a[i] * (i % 100 + 1)) % 1000000007L;
    return cs;
}
int main() {
    std::atomic<long> acc{0};
    std::thread ts[4];
    for (long b = 0; b < 4; b++) ts[b] = std::thread([&acc, b]{ acc.fetch_add(worker(b)); });
    for (auto& t : ts) t.join();
    printf("%ld\n", acc.load());
}
