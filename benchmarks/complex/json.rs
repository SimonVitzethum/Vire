fn gen(buf: &mut [i64], p: &mut i64, depth: i64, s: &mut i64) {
    *s = (*s * 1103515245 + 12345) % 2147483648;
    let mut choice = *s % 5;
    if depth <= 0 { choice = 0; }
    if *p > 3999000 { choice = 0; }
    if choice == 1 || choice == 2 {
        buf[*p as usize] = 91; *p += 1;
        let cnt = *s % 2 + 1;
        for k in 0..cnt { if k > 0 { buf[*p as usize] = 44; *p += 1; } gen(buf, p, depth-1, s); }
        buf[*p as usize] = 93; *p += 1;
    } else if choice == 3 || choice == 4 {
        buf[*p as usize] = 123; *p += 1;
        let cnt = *s % 2 + 1;
        for k in 0..cnt {
            if k > 0 { buf[*p as usize] = 44; *p += 1; }
            buf[*p as usize] = 34; *p += 1;
            buf[*p as usize] = 107 + k; *p += 1;
            buf[*p as usize] = 34; *p += 1;
            buf[*p as usize] = 58; *p += 1;
            gen(buf, p, depth-1, s);
        }
        buf[*p as usize] = 125; *p += 1;
    } else {
        *s = (*s * 1103515245 + 12345) % 2147483648;
        let num = *s % 900 + 100;
        buf[*p as usize] = num/100+48; *p += 1;
        buf[*p as usize] = num/10%10+48; *p += 1;
        buf[*p as usize] = num%10+48; *p += 1;
    }
}
fn parse(buf: &[i64], p: &mut i64) -> i64 {
    let c = buf[*p as usize];
    if c == 91 {
        *p += 1; let mut sum = 0i64;
        while buf[*p as usize] != 93 { if buf[*p as usize] == 44 { *p += 1; } else { sum = (sum + parse(buf, p)) % 1000000007; } }
        *p += 1; sum
    } else if c == 123 {
        *p += 1; let mut sum = 0i64;
        while buf[*p as usize] != 125 {
            let c2 = buf[*p as usize];
            if c2 == 34 { *p += 4; } else if c2 == 44 { *p += 1; } else { sum = (sum + parse(buf, p)) % 1000000007; }
        }
        *p += 1; sum
    } else {
        let mut v = 0i64;
        while buf[*p as usize] >= 48 && buf[*p as usize] <= 57 { v = v*10 + (buf[*p as usize] - 48); *p += 1; }
        v
    }
}
fn main() {
    let mut buf = vec![0i64; 4000010];
    let mut checksum = 0i64;
    for it in 0..40i64 {
        let mut s = (it * 2654435761 + 999) % 2147483648;
        let mut p = 0i64;
        gen(&mut buf, &mut p, 15, &mut s);
        let mut p2 = 0i64;
        checksum = (checksum + parse(&buf, &mut p2)) % 1000000007;
    }
    println!("{}", checksum);
}
