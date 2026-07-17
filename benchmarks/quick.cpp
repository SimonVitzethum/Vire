#include <cstdio>
#include <cstdint>
#include <vector>
void sort(int* a, int lo, int hi) {
    while (lo < hi) {
        int p = a[(unsigned)(lo + hi) >> 1], i = lo, j = hi;
        while (i <= j) {
            while (a[i] < p) i++;
            while (a[j] > p) j--;
            if (i <= j) { int t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
        }
        if (j - lo < hi - i) { sort(a, lo, j); lo = i; } else { sort(a, i, hi); hi = j; }
    }
}
int main() {
    int n = 20000000; std::vector<int> a(n);
    uint64_t s = 12345;
    for (int i = 0; i < n; i++) { s = s * 6364136223846793005ULL + 1442695040888963407ULL; a[i] = (int)(s >> 33); }
    sort(a.data(), 0, n - 1);
    int64_t sum = 0; for (int i = 0; i < n; i += 1000) sum += a[i];
    printf("%lld %d %d\n", (long long)sum, a[0], a[n-1]);
}
