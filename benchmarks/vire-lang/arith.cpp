#include <cstdio>
#include <cstdint>
int main() {
    int64_t s = 0;
    for (int64_t i = 0; i < 300000000; i++)
        s = (s + i * 3 + 7) % 1000000007;
    printf("%ld\n", s);
}
