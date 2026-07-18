#include <cstdio>
#include <cstdint>
struct V { int64_t x, y, z; };
int main() {
    int64_t s = 0;
    for (int64_t i = 0; i < 100000000; i++) {
        V v{i, i*2, i*3};
        s = (s + v.x + v.y + v.z) % 1000000007;
    }
    printf("%ld\n", s);
}
