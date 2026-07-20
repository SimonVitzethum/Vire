#include <cstdio>
#include <vector>
using std::vector;
static long matchhere(const long* pat, long pi, long pl, const vector<long>& tx, long ti, long tl);
static long matchstar(long c, const long* pat, long pi, long pl, const vector<long>& tx, long ti, long tl){
    long t=ti; for(;;){ if(matchhere(pat,pi,pl,tx,t,tl)==1) return 1; if(t<tl && (c==46||c==tx[t])) t++; else return 0; }
}
static long matchhere(const long* pat, long pi, long pl, const vector<long>& tx, long ti, long tl){
    if(pi>=pl) return 1;
    if(pi+1<pl && pat[pi+1]==42) return matchstar(pat[pi],pat,pi+2,pl,tx,ti,tl);
    if(ti<tl && (pat[pi]==46||pat[pi]==tx[ti])) return matchhere(pat,pi+1,pl,tx,ti+1,tl);
    return 0;
}
static long search(const long* pat, long pl, const vector<long>& tx, long tl){
    for(long ti=0; ti<=tl; ti++) if(matchhere(pat,0,pl,tx,ti,tl)==1) return 1;
    return 0;
}
int main(){
    long pat[16]={97,46,42,98,46,42,99,46,42,100,46,42,97,46,42,98};
    long pl=16, tl=40; vector<long> tx(tl);
    long seed=20240101, count=0, n=2000000;
    for(long i=0;i<n;i++){ for(long j=0;j<tl;j++){ seed=(seed*1103515245+12345)%2147483648L; tx[j]=97+seed%4; } count+=search(pat,pl,tx,tl); }
    printf("%ld\n", count);
}
