#include <cstdio>
#include <cstdint>
struct Tree { Tree *l, *r; };
Tree* make(int64_t d){ Tree* t=new Tree; if(d==0){t->l=t->r=nullptr;} else {t->l=make(d-1); t->r=make(d-1);} return t; }
int64_t check(Tree* t, int64_t d){ return d==0?1:1+check(t->l,d-1)+check(t->r,d-1); }
void del(Tree* t){ if(t->l){del(t->l); del(t->r);} delete t; }
int main(){ int64_t sum=0; for(int i=0;i<60;i++){ Tree* t=make(16); sum+=check(t,16); del(t);} printf("%ld\n",sum); }
