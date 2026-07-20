use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::thread;
fn qsort(a: &mut [i64], lo: i64, hi: i64) {
    if lo < hi {
        let p = a[((lo + hi) / 2) as usize];
        let (mut i, mut j) = (lo, hi);
        while i <= j {
            while a[i as usize] < p { i += 1; }
            while a[j as usize] > p { j -= 1; }
            if i <= j { a.swap(i as usize, j as usize); i += 1; j -= 1; }
        }
        qsort(a, lo, j);
        qsort(a, i, hi);
    }
}
fn worker(base: i64) -> i64 {
    let n = 1000000usize;
    let mut a = vec![0i64; n];
    let mut seed = base * 2654435761 + 12345;
    for i in 0..n { seed = (seed * 1103515245 + 12345) % 2147483648; a[i] = seed % 1000000; }
    qsort(&mut a, 0, n as i64 - 1);
    let mut cs = 0i64;
    for i in 0..n { cs = (cs + a[i] * ((i % 100) as i64 + 1)) % 1000000007; }
    cs
}
fn main() {
    let acc = Arc::new(AtomicI64::new(0));
    let mut hs = vec![];
    for b in 0..4i64 { let a = acc.clone(); hs.push(thread::spawn(move || { a.fetch_add(worker(b), Ordering::Relaxed); })); }
    for t in hs { t.join().unwrap(); }
    println!("{}", acc.load(Ordering::Relaxed));
}
