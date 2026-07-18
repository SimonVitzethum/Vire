#include <cstdio>
#include <cstdint>
struct Op { virtual int64_t apply(int64_t x)=0; virtual ~Op(){} };
struct AddOp:Op { int64_t k; AddOp(int64_t k):k(k){} int64_t apply(int64_t x)override{return x+k;} };
struct MulOp:Op { int64_t k; MulOp(int64_t k):k(k){} int64_t apply(int64_t x)override{return x*k;} };
int64_t run(Op*o,int64_t iters){int64_t acc=0;for(int64_t i=0;i<iters;i++)acc=o->apply(acc)%1000000;return acc;}
int main(){ AddOp a(3); MulOp m(7); printf("%ld\n",run(&a,50000000)+run(&m,50000000)); }
