fn main() {
    let (vn, deg) = (200000i64, 8i64);
    let m = (vn * deg) as usize;
    let (mut dst, mut wt) = (vec![0i64; m], vec![0i64; m]);
    let mut seed = 2166136261i64;
    for i in 0..m { seed=(seed*1103515245+12345)%2147483648; dst[i]=seed%vn; seed=(seed*1103515245+12345)%2147483648; wt[i]=seed%100+1; }
    let mut level = vec![-1i64; vn as usize];
    let mut queue = vec![0i64; vn as usize];
    let (mut head, mut tail) = (0usize, 0usize);
    level[0]=0; queue[tail]=0; tail+=1;
    while head < tail {
        let u = queue[head]; head+=1;
        for j in (u*deg)..(u*deg+deg) { let v=dst[j as usize]; if level[v as usize]<0 { level[v as usize]=level[u as usize]+1; queue[tail]=v; tail+=1; } }
    }
    let mut bsum=0i64; for i in 0..vn as usize { if level[i]>0 { bsum=(bsum+level[i])%1000000007; } }
    let mut dist = vec![2000000000i64; vn as usize];
    let (mut hd, mut hn) = (vec![0i64; m+16], vec![0i64; m+16]);
    let mut hs = 0usize;
    dist[0]=0; hd[0]=0; hn[0]=0; hs=1;
    while hs > 0 {
        let cd=hd[0]; let cu=hn[0]; hs-=1; hd[0]=hd[hs]; hn[0]=hn[hs];
        let mut p=0usize;
        loop {
            let l=p*2+1; let r=l+1; let mut sm=p;
            if l<hs && hd[l]<hd[sm] { sm=l; }
            if r<hs && hd[r]<hd[sm] { sm=r; }
            if sm==p { break; }
            hd.swap(p,sm); hn.swap(p,sm); p=sm;
        }
        if cd <= dist[cu as usize] {
            for j in (cu*deg)..(cu*deg+deg) {
                let v=dst[j as usize]; let nd=cd+wt[j as usize];
                if nd < dist[v as usize] {
                    dist[v as usize]=nd; hd[hs]=nd; hn[hs]=v; let mut c=hs; hs+=1;
                    while c>0 { let par=(c-1)/2; if hd[par]<=hd[c] { break; } hd.swap(par,c); hn.swap(par,c); c=par; }
                }
            }
        }
    }
    let mut dsum=0i64; for i in 0..vn as usize { if dist[i]<2000000000 { dsum=(dsum+dist[i])%1000000007; } }
    println!("{}", bsum*1000 + dsum%1000000007);
}
