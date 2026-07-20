#include <cstdio>
#include <vector>
using std::vector;
int main() {
    int n = 50000, kk = 16;
    vector<long> xs(n), ys(n); long seed = 987654321;
    for (int i = 0; i < n; i++) {
        seed = (seed * 1103515245 + 12345) % 2147483648L; xs[i] = seed % 1000;
        seed = (seed * 1103515245 + 12345) % 2147483648L; ys[i] = seed % 1000;
    }
    vector<long> cx(kk), cy(kk);
    for (int i = 0; i < kk; i++) { cx[i] = xs[i * 137 % n]; cy[i] = ys[i * 137 % n]; }
    vector<long> sumx(kk), sumy(kk), cnt(kk);
    for (int it = 0; it < 25; it++) {
        for (int c = 0; c < kk; c++) { sumx[c] = 0; sumy[c] = 0; cnt[c] = 0; }
        for (int i = 0; i < n; i++) {
            long px = xs[i], py = ys[i]; int best = 0; long bestd = 2000000000L;
            for (int c = 0; c < kk; c++) { long dx = px - cx[c], dy = py - cy[c], d = dx * dx + dy * dy; if (d < bestd) { bestd = d; best = c; } }
            sumx[best] += px; sumy[best] += py; cnt[best]++;
        }
        for (int c = 0; c < kk; c++) if (cnt[c] > 0) { cx[c] = sumx[c] / cnt[c]; cy[c] = sumy[c] / cnt[c]; }
    }
    long checksum = 0;
    for (int i = 0; i < kk; i++) checksum += cx[i] * 31 + cy[i];
    printf("%ld\n", checksum);
}
