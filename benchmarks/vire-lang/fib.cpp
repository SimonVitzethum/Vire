#include <cstdio>
#include <cstdint>
int64_t fib(int64_t n){ return n<2?n:fib(n-1)+fib(n-2); }
int main(){ printf("%ld\n", fib(38)); }
