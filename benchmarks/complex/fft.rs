fn modpow(b: i64, e: i64, m: i64) -> i64 {
    let (mut r, mut bb, mut ee) = (1i64, b % m, e);
    while ee > 0 { if ee % 2 == 1 { r = r * bb % m; } bb = bb * bb % m; ee /= 2; }
    r
}
fn main() {
    let n = 1048576usize; let md = 998244353i64;
    let mut a = vec![0i64; n]; let mut seed = 123456789i64;
    for i in 0..n { seed = (seed * 1103515245 + 12345) % 2147483648; a[i] = seed % md; }
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n / 2;
        while j >= bit { j -= bit; bit /= 2; }
        j += bit;
        if i < j { a.swap(i, j); }
    }
    let mut len = 2usize;
    while len <= n {
        let wlen = modpow(3, (md - 1) / len as i64, md);
        let half = len / 2;
        let mut i = 0usize;
        while i < n {
            let mut w = 1i64;
            for k in 0..half {
                let u = a[i + k];
                let v = a[i + k + half] * w % md;
                let mut s = u + v; if s >= md { s -= md; }
                let mut d = u - v; if d < 0 { d += md; }
                a[i + k] = s; a[i + k + half] = d;
                w = w * wlen % md;
            }
            i += len;
        }
        len *= 2;
    }
    let mut checksum = 0i64;
    for i in 0..n { checksum = (checksum + a[i] * ((i % 97) as i64 + 1)) % 1000000007; }
    println!("{}", checksum);
}
