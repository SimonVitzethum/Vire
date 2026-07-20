#include <cstdio>
#include <vector>
using std::vector;
static void gen(vector<long>& buf, long& p, long depth, long& s){
    s=(s*1103515245+12345)%2147483648L; long choice=s%5;
    if(depth<=0)choice=0; if(p>3999000)choice=0;
    if(choice==1||choice==2){ buf[p++]=91; long cnt=s%2+1; for(long k=0;k<cnt;k++){ if(k>0)buf[p++]=44; gen(buf,p,depth-1,s); } buf[p++]=93; }
    else if(choice==3||choice==4){ buf[p++]=123; long cnt=s%2+1; for(long k=0;k<cnt;k++){ if(k>0)buf[p++]=44; buf[p++]=34; buf[p++]=107+k; buf[p++]=34; buf[p++]=58; gen(buf,p,depth-1,s); } buf[p++]=125; }
    else { s=(s*1103515245+12345)%2147483648L; long num=s%900+100; buf[p++]=num/100+48; buf[p++]=num/10%10+48; buf[p++]=num%10+48; }
}
static long parse(const vector<long>& buf, long& p){
    long c=buf[p];
    if(c==91){ p++; long sum=0; while(buf[p]!=93){ if(buf[p]==44)p++; else sum=(sum+parse(buf,p))%1000000007L; } p++; return sum; }
    else if(c==123){ p++; long sum=0; while(buf[p]!=125){ long c2=buf[p]; if(c2==34)p+=4; else if(c2==44)p++; else sum=(sum+parse(buf,p))%1000000007L; } p++; return sum; }
    else { long v=0; while(buf[p]>=48 && buf[p]<=57){ v=v*10+(buf[p]-48); p++; } return v; }
}
int main(){
    vector<long> buf(4000010); long checksum=0;
    for(long it=0;it<40;it++){ long s=(it*2654435761L+999)%2147483648L, p=0; gen(buf,p,15,s); long p2=0; checksum=(checksum+parse(buf,p2))%1000000007L; }
    printf("%ld\n", checksum);
}
