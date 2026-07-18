// Matched Rust-indices baseline for benchmarks/vire-m0/pagerank.vr:
// doubly-linked ring via Vec indices (no RC, no collector), same in-place
// neighbour-average update over 40 iterations. The M0.1 "Rust indices" column.
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let mut next = vec![0usize; n];
    let mut prev = vec![0usize; n];
    let mut rank = vec![1i64; n];
    for i in 0..n {
        next[i] = (i + 1) % n;
        prev[i] = (i + n - 1) % n;
    }
    for _ in 0..40 {
        for i in 0..n {
            rank[i] = (rank[prev[i]] + rank[next[i]]) / 2 + 1;
        }
    }
    let s: i64 = rank.iter().sum();
    println!("{}", s);
}
