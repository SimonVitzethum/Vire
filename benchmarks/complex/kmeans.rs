fn main() {
    let (n, kk) = (50000usize, 16usize);
    let (mut xs, mut ys) = (vec![0i64; n], vec![0i64; n]);
    let mut seed = 987654321i64;
    for i in 0..n {
        seed = (seed * 1103515245 + 12345) % 2147483648; xs[i] = seed % 1000;
        seed = (seed * 1103515245 + 12345) % 2147483648; ys[i] = seed % 1000;
    }
    let (mut cx, mut cy) = (vec![0i64; kk], vec![0i64; kk]);
    for i in 0..kk { cx[i] = xs[i * 137 % n]; cy[i] = ys[i * 137 % n]; }
    let (mut sumx, mut sumy, mut cnt) = (vec![0i64; kk], vec![0i64; kk], vec![0i64; kk]);
    for _ in 0..25 {
        for c in 0..kk { sumx[c] = 0; sumy[c] = 0; cnt[c] = 0; }
        for i in 0..n {
            let (px, py) = (xs[i], ys[i]);
            let (mut best, mut bestd) = (0usize, 2000000000i64);
            for c in 0..kk {
                let (dx, dy) = (px - cx[c], py - cy[c]);
                let d = dx * dx + dy * dy;
                if d < bestd { bestd = d; best = c; }
            }
            sumx[best] += px; sumy[best] += py; cnt[best] += 1;
        }
        for c in 0..kk { if cnt[c] > 0 { cx[c] = sumx[c] / cnt[c]; cy[c] = sumy[c] / cnt[c]; } }
    }
    let mut checksum = 0i64;
    for i in 0..kk { checksum += cx[i] * 31 + cy[i]; }
    println!("{}", checksum);
}
