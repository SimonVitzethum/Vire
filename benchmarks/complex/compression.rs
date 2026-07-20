fn rd4(d: &[i64], p: i64) -> i64 { d[p as usize] + d[(p+1) as usize]*256 + d[(p+2) as usize]*65536 + d[(p+3) as usize]*16777216 }
fn matchlen(d: &[i64], cand: i64, pos: i64, n: i64) -> i64 {
    let mut ml = 0i64;
    while pos + ml < n { if d[(cand+ml) as usize] == d[(pos+ml) as usize] { ml += 1; } else { return ml; } }
    ml
}
fn main() {
    let n = 4194304i64;
    let mut d = vec![0i64; n as usize];
    let mut block = vec![0i64; 1024];
    let mut seed = 777i64;
    for i in 0..1024usize { seed = (seed*1103515245+12345)%2147483648; block[i] = seed%256; }
    for i in 0..n as usize {
        if i % 64 == 0 { seed = (seed*1103515245+12345)%2147483648; d[i] = seed%256; } else { d[i] = block[i%1024]; }
    }
    let tsize = 65536i64;
    let mut table = vec![-1i64; tsize as usize];
    let (mut pos, mut lits, mut matches) = (0i64, 0i64, 0i64);
    let lim = n - 4;
    while pos < lim {
        let h = (d[pos as usize] + d[(pos+1) as usize]*251 + d[(pos+2) as usize]*63001 + d[(pos+3) as usize]*15813251) % tsize;
        let cand = table[h as usize];
        table[h as usize] = pos;
        if cand >= 0 && rd4(&d, cand) == rd4(&d, pos) {
            let ml = matchlen(&d, cand, pos, n);
            matches += 1; pos += ml;
        } else { lits += 1; pos += 1; }
    }
    println!("{}", lits + matches*3);
}
