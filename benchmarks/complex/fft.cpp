#include <cstdio>
#include <vector>
using std::vector;
static long modpow(long b, long e, long m) {
    long r = 1, bb = b % m, ee = e;
    while (ee > 0) { if (ee % 2 == 1) r = r * bb % m; bb = bb * bb % m; ee /= 2; }
    return r;
}
int main() {
    long n = 1048576, md = 998244353;
    vector<long> a(n); long seed = 123456789;
    for (long i = 0; i < n; i++) { seed = (seed * 1103515245 + 12345) % 2147483648L; a[i] = seed % md; }
    long j = 0;
    for (long i = 1; i < n; i++) {
        long bit = n / 2;
        while (j >= bit) { j -= bit; bit /= 2; }
        j += bit;
        if (i < j) { long t = a[i]; a[i] = a[j]; a[j] = t; }
    }
    for (long len = 2; len <= n; len *= 2) {
        long wlen = modpow(3, (md - 1) / len, md), half = len / 2;
        for (long i = 0; i < n; i += len) {
            long w = 1;
            for (long k = 0; k < half; k++) {
                long u = a[i + k], v = a[i + k + half] * w % md;
                long s = u + v; if (s >= md) s -= md;
                long d = u - v; if (d < 0) d += md;
                a[i + k] = s; a[i + k + half] = d;
                w = w * wlen % md;
            }
        }
    }
    long checksum = 0;
    for (long i = 0; i < n; i++) checksum = (checksum + a[i] * (i % 97 + 1)) % 1000000007L;
    printf("%ld\n", checksum);
}
