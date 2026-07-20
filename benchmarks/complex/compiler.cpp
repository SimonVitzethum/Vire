#include <cstdio>
#include <vector>
using std::vector;
struct Node { long kind, val; Node *l, *r; };
static vector<Node*> pool;
static Node* mk(long k, long v, Node* l, Node* r){ Node* n=new Node{k,v,l,r}; pool.push_back(n); return n; }
static void gen(vector<long>& buf, long& p, long depth, long& s){
    s=(s*1103515245+12345)%2147483648L;
    if(depth<=0 || s%3==0){ long num=s%90+10; buf[p++]=num/10+48; buf[p++]=num%10+48; }
    else { buf[p++]=40; gen(buf,p,depth-1,s); s=(s*1103515245+12345)%2147483648L; long op=43; if(s%3==1)op=45; if(s%3==2)op=42; buf[p++]=op; gen(buf,p,depth-1,s); buf[p++]=41; }
}
static Node* parse(const vector<long>& buf, long& p){
    long c=buf[p];
    if(c==40){ p++; Node* l=parse(buf,p); long op=buf[p++]; Node* r=parse(buf,p); p++; return mk(op,0,l,r); }
    else { long v=0; while(buf[p]>=48 && buf[p]<=57){ v=v*10+(buf[p]-48); p++; } return mk(0,v,nullptr,nullptr); }
}
static long eval(Node* n){
    if(n->kind==0) return n->val;
    long a=eval(n->l), b=eval(n->r);
    if(n->kind==43) return (a+b)%1000000007L;
    if(n->kind==45){ long x=(a-b)%1000000007L; if(x<0)x+=1000000007L; return x; }
    return a*b%1000000007L;
}
int main(){
    vector<long> buf(2000000); long checksum=0;
    for(long it=0;it<400;it++){
        long s=it*2654435761L+12345, p=0;
        gen(buf,p,15,s);
        long p2=0; Node* ast=parse(buf,p2);
        checksum=(checksum+eval(ast))%1000000007L;
        for(Node* nd:pool) delete nd; pool.clear();
    }
    printf("%ld\n", checksum);
}
