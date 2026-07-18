#include <cstdio>
#include <cstdint>
int main(){ int64_t s=0; for(int64_t i=0;i<30000000;i++){ int64_t x=i,c=0; while(x>0){c+=x&1;x/=2;} s+=c;} printf("%ld\n",s); }
