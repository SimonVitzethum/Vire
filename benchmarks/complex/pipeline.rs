fn qsort(a: &mut [i64], lo: i64, hi: i64) {
    if lo < hi {
        let p = a[hi as usize];
        let mut i = lo - 1;
        let mut j = lo;
        while j < hi { if a[j as usize] < p { i += 1; a.swap(i as usize, j as usize); } j += 1; }
        a.swap((i + 1) as usize, hi as usize);
        qsort(a, lo, i);
        qsort(a, i + 2, hi);
    }
}
fn bsearch(a: &[i64], n: i64, key: i64) -> i64 {
    let (mut lo, mut hi) = (0i64, n - 1);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        if a[mid as usize] == key { return mid; }
        if a[mid as usize] < key { lo = mid + 1; } else { hi = mid - 1; }
    }
    -1
}
fn main() {
    let n = 200000i64;
    let mut a = vec![0i64; n as usize];
    let mut seed = 12345i64;
    for i in 0..n as usize { seed = (seed * 1103515245 + 12345) % 2147483648; a[i] = seed % 1000000; }
    qsort(&mut a, 0, n - 1);
    let mut hits = 0i64;
    for q in 0..20000i64 { if bsearch(&a, n, q * 50) >= 0 { hits += 1; } }
    let mut hist = [0i64; 256];
    for i in 0..n as usize { let b = (a[i] % 256) as usize; hist[b] += 1; }
    let mut checksum = 0i64;
    for k in 0..256i64 { checksum = (checksum + hist[k as usize] * (k + 1)) % 1000000007; }
    println!("{}", hits * 1000000007 % 1000000007 + checksum * 1000 + hits);
}
