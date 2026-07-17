fn main() {
    let n = 4000i64; let mut sum: i64 = 0;
    for py in 0..n {
        let y0 = (py as f64) * 2.0 / (n as f64) - 1.0;
        for px in 0..n {
            let x0 = (px as f64) * 2.5 / (n as f64) - 2.0;
            let mut x = 0.0f64; let mut y = 0.0f64; let mut it = 0i64;
            while x*x + y*y <= 4.0 && it < 100 {
                let xt = x*x - y*y + x0;
                y = 2.0*x*y + y0; x = xt; it += 1;
            }
            sum += it;
        }
    }
    println!("{}", sum);
}
