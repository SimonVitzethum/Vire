struct Node { rank: f64, next: f64, outdeg: usize, out: Vec<usize> }
fn main() {
    let n = 200_000usize; let e = 6usize;
    let mut nodes: Vec<Node> = (0..n).map(|_| Node{rank:1.0/n as f64, next:0.0, outdeg:0, out:Vec::new()}).collect();
    let mut s: u64 = 88172645463325252;
    for i in 0..n {
        let mut out = Vec::with_capacity(e);
        for _ in 0..e { s ^= s<<13; s ^= s>>7; s ^= s<<17; out.push(((s>>1)%(n as u64)) as usize); }
        nodes[i].out = out; nodes[i].outdeg = e;
    }
    let d = 0.85; let base = (1.0-d)/n as f64;
    for _ in 0..40 {
        for i in 0..n { nodes[i].next = base; }
        for i in 0..n {
            let share = d * nodes[i].rank / nodes[i].outdeg as f64;
            let m = nodes[i].out.len();
            for k in 0..m { let j = nodes[i].out[k]; nodes[j].next += share; }
        }
        for i in 0..n { nodes[i].rank = nodes[i].next; }
    }
    let mut sum = 0.0f64; for i in 0..n { sum += nodes[i].rank; }
    println!("{}", (sum * 1_000_000.0) as i64);
}
