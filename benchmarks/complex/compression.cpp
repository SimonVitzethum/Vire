#include <cstdio>
#include <vector>
using std::vector;
static long rd4(const vector<long>& d, long p){ return d[p]+d[p+1]*256+d[p+2]*65536+d[p+3]*16777216; }
static long matchlen(const vector<long>& d, long cand, long pos, long n){ long ml=0; while(pos+ml<n){ if(d[cand+ml]==d[pos+ml]) ml++; else return ml; } return ml; }
int main(){
    long n=4194304; vector<long> d(n), block(1024); long seed=777;
    for(int i=0;i<1024;i++){ seed=(seed*1103515245+12345)%2147483648L; block[i]=seed%256; }
    for(long i=0;i<n;i++){ if(i%64==0){ seed=(seed*1103515245+12345)%2147483648L; d[i]=seed%256; } else d[i]=block[i%1024]; }
    long tsize=65536; vector<long> table(tsize,-1);
    long pos=0,lits=0,matches=0,lim=n-4;
    while(pos<lim){
        long h=(d[pos]+d[pos+1]*251+d[pos+2]*63001+d[pos+3]*15813251)%tsize;
        long cand=table[h]; table[h]=pos;
        if(cand>=0 && rd4(d,cand)==rd4(d,pos)){ long ml=matchlen(d,cand,pos,n); matches++; pos+=ml; }
        else { lits++; pos++; }
    }
    printf("%ld\n", lits+matches*3);
}
