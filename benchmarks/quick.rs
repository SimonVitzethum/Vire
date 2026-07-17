fn sort(a: &mut [i32], mut lo: i32, mut hi: i32) {
    while lo < hi {
        let p = a[((lo + hi) >> 1) as usize]; let (mut i, mut j) = (lo, hi);
        while i <= j {
            while a[i as usize] < p { i += 1; }
            while a[j as usize] > p { j -= 1; }
            if i <= j { a.swap(i as usize, j as usize); i += 1; j -= 1; }
        }
        if j - lo < hi - i { sort(a, lo, j); lo = i; } else { sort(a, i, hi); hi = j; }
    }
}
fn main() {
    let n = 20_000_000i32; let mut a = vec![0i32; n as usize];
    let mut s: u64 = 12345;
    for i in 0..n as usize { s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); a[i] = (s >> 33) as i32; }
    sort(&mut a, 0, n - 1);
    let mut sum: i64 = 0; let mut i = 0usize; while i < n as usize { sum += a[i] as i64; i += 1000; }
    println!("{} {} {}", sum, a[0], a[(n-1) as usize]);
}
