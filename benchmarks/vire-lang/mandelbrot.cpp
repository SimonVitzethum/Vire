#include <cstdio>
#include <cstdint>
int main() {
    int64_t n = 2000, count = 0;
    for (int64_t py = 0; py < n; py++) for (int64_t px = 0; px < n; px++) {
        double cr = px * 3.0 / n - 2.0, ci = py * 2.0 / n - 1.0, zr = 0, zi = 0;
        int64_t esc = 0, i = 0;
        while (i < 50) { double zr2 = zr*zr - zi*zi + cr; zi = 2.0*zr*zi + ci; zr = zr2;
            if (zr*zr + zi*zi > 4.0) { esc = 1; i = 50; } else i++; }
        count += 1 - esc;
    }
    printf("%ld\n", count);
}
