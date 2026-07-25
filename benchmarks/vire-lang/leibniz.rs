// Leibniz series for pi (100M terms) — matches leibniz.vr / leibniz.cpp.
fn main() {
    let mut sum: f64 = 0.0;
    let mut denom: f64 = 1.0;
    let mut sign: f64 = 1.0;
    let mut k: i64 = 0;
    while k < 100000000 {
        sum += sign / denom;
        denom += 2.0;
        sign = -sign;
        k += 1;
    }
    let pi = sum * 4.0;
    println!("{}", (pi * 1000000.0) as i64);
}
