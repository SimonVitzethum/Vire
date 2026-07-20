use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::thread;
fn band(base: i64) -> i64 {
    let mut sum = 0i64;
    for y in base * 500..base * 500 + 500 {
        for px in 0..2000i64 {
            let cr = px as f64 * 3.0 / 2000.0 - 2.0;
            let ci = y as f64 * 3.0 / 2000.0 - 1.5;
            let (mut zr, mut zi) = (0.0f64, 0.0f64);
            let mut it = 0i64;
            for _ in 0..200 {
                let (zr2, zi2) = (zr * zr, zi * zi);
                if zr2 + zi2 > 4.0 { break; }
                let nzr = zr2 - zi2 + cr;
                zi = 2.0 * zr * zi + ci;
                zr = nzr;
                it += 1;
            }
            sum += it;
        }
    }
    sum
}
fn main() {
    let total = Arc::new(AtomicI64::new(0));
    let mut hs = vec![];
    for b in 0..4i64 {
        let t = total.clone();
        hs.push(thread::spawn(move || { t.fetch_add(band(b), Ordering::Relaxed); }));
    }
    for t in hs { t.join().unwrap(); }
    println!("{}", total.load(Ordering::Relaxed));
}
