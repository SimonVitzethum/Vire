fn lookup(keys: &[i64], vals: &[i64], cap: i64, key: i64) -> i64 {
    let mut idx = (key * 2654435761) % 2147483648 % cap;
    let mut steps = 0;
    while steps < cap {
        let k = keys[idx as usize];
        if k == 0 { return 0; }
        if k == key { return vals[idx as usize]; }
        idx += 1; if idx >= cap { idx = 0; }
        steps += 1;
    }
    0
}
fn insert(keys: &mut [i64], vals: &mut [i64], cap: i64, key: i64, val: i64) {
    let mut idx = (key * 2654435761) % 2147483648 % cap;
    while keys[idx as usize] > 0 { idx += 1; if idx >= cap { idx = 0; } }
    keys[idx as usize] = key; vals[idx as usize] = val;
}
fn remove(keys: &mut [i64], cap: i64, key: i64) {
    let mut idx = (key * 2654435761) % 2147483648 % cap;
    let mut steps = 0;
    while steps < cap {
        let k = keys[idx as usize];
        if k == 0 { return; }
        if k == key { keys[idx as usize] = -1; return; }
        idx += 1; if idx >= cap { idx = 0; }
        steps += 1;
    }
}
fn main() {
    let cap = 1048576i64;
    let mut keys = vec![0i64; cap as usize];
    let mut vals = vec![0i64; cap as usize];
    let n = 400000i64;
    let mut i = 1; while i <= n { insert(&mut keys, &mut vals, cap, i, i * 3); i += 1; }
    let mut checksum = 0i64;
    let mut q = 1; while q <= 800000 { let key = q * 7919 % 800000 + 1; checksum = (checksum + lookup(&keys, &vals, cap, key)) % 1000000007; q += 1; }
    i = 2; while i <= n { remove(&mut keys, cap, i); i += 2; }
    q = 1; while q <= 800000 { let key = q * 7919 % 800000 + 1; checksum = (checksum + lookup(&keys, &vals, cap, key)) % 1000000007; q += 1; }
    println!("{}", checksum);
}
