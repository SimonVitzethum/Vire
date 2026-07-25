// Leibniz series for pi (100M terms) — matches leibniz.vr / leibniz.rs.
#include <cstdio>
#include <cstdint>
int main() {
    double sum = 0.0, denom = 1.0, sign = 1.0;
    for (int64_t k = 0; k < 100000000; k++) {
        sum += sign / denom;
        denom += 2.0;
        sign = -sign;
    }
    double pi = sum * 4.0;
    printf("%lld\n", (long long)(pi * 1000000.0));
    return 0;
}
