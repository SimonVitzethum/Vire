use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::thread;
fn sample(base: i64) -> i64 {
    let mut seed = base * 2654435761 + 1;
    let mut count = 0i64;
    for _ in 0..25000000i64 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let x = seed % 32768;
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let y = seed % 32768;
        if x * x + y * y <= 1073676289 { count += 1; }
    }
    count
}
fn main() {
    let hits = Arc::new(AtomicI64::new(0));
    let mut hs = vec![];
    for b in 0..4i64 {
        let h = hits.clone();
        hs.push(thread::spawn(move || { h.fetch_add(sample(b), Ordering::Relaxed); }));
    }
    for t in hs { t.join().unwrap(); }
    println!("{}", hits.load(Ordering::Relaxed));
}
