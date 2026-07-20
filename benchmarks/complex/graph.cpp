#include <cstdio>
#include <vector>
using std::vector;
int main() {
    long vn=200000, deg=8, m=vn*deg;
    vector<long> dst(m), wt(m); long seed=2166136261;
    for (long i=0;i<m;i++){seed=(seed*1103515245+12345)%2147483648L;dst[i]=seed%vn;seed=(seed*1103515245+12345)%2147483648L;wt[i]=seed%100+1;}
    vector<long> level(vn,-1), queue(vn); long head=0,tail=0;
    level[0]=0; queue[tail++]=0;
    while(head<tail){ long u=queue[head++]; for(long j=u*deg;j<u*deg+deg;j++){long v=dst[j]; if(level[v]<0){level[v]=level[u]+1;queue[tail++]=v;}} }
    long bsum=0; for(long i=0;i<vn;i++) if(level[i]>0) bsum=(bsum+level[i])%1000000007L;
    vector<long> dist(vn,2000000000), hd(m+16), hn(m+16); long hs=0;
    dist[0]=0; hd[0]=0; hn[0]=0; hs=1;
    while(hs>0){
        long cd=hd[0],cu=hn[0]; hs--; hd[0]=hd[hs]; hn[0]=hn[hs];
        long p=0;
        while(true){ long l=p*2+1,r=l+1,sm=p; if(l<hs&&hd[l]<hd[sm])sm=l; if(r<hs&&hd[r]<hd[sm])sm=r; if(sm==p)break; long t=hd[p];hd[p]=hd[sm];hd[sm]=t; t=hn[p];hn[p]=hn[sm];hn[sm]=t; p=sm; }
        if(cd<=dist[cu]){ for(long j=cu*deg;j<cu*deg+deg;j++){ long v=dst[j],nd=cd+wt[j]; if(nd<dist[v]){ dist[v]=nd; hd[hs]=nd; hn[hs]=v; long c=hs; hs++; while(c>0){long par=(c-1)/2; if(hd[par]<=hd[c])break; long t=hd[par];hd[par]=hd[c];hd[c]=t; t=hn[par];hn[par]=hn[c];hn[c]=t; c=par;} } } }
    }
    long dsum=0; for(long i=0;i<vn;i++) if(dist[i]<2000000000) dsum=(dsum+dist[i])%1000000007L;
    printf("%ld\n", bsum*1000 + dsum%1000000007L);
}
