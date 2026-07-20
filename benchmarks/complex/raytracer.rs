fn main() {
    let (w, h) = (2400i64, 2400i64);
    let scx = [0.0f64, 2.0, -2.0, 0.0];
    let scy = [0.0f64, 0.0, 1.0, -1.0];
    let scz = [5.0f64, 6.0, 5.5, 4.0];
    let sr  = [1.0f64, 1.2, 0.8, 0.6];
    let (lx, ly, lz) = (5.0f64, 5.0, -2.0);
    let mut total = 0i64;
    for py in 0..h {
        for px in 0..w {
            let ddx = px as f64 / w as f64 - 0.5;
            let ddy = py as f64 / h as f64 - 0.5;
            let ddz = 1.0f64;
            let rl = (ddx*ddx + ddy*ddy + ddz*ddz).sqrt();
            let (dx, dy, dz) = (ddx/rl, ddy/rl, ddz/rl);
            let mut best = 1000000.0f64;
            let mut hit = -1i64;
            for s in 0..4usize {
                let (ocx, ocy, ocz) = (-scx[s], -scy[s], -scz[s]);
                let b = ocx*dx + ocy*dy + ocz*dz;
                let c = ocx*ocx + ocy*ocy + ocz*ocz - sr[s]*sr[s];
                let disc = b*b - c;
                if disc > 0.0 {
                    let t = -b - disc.sqrt();
                    if t > 0.001 && t < best { best = t; hit = s as i64; }
                }
            }
            let mut bright = 0i64;
            if hit >= 0 {
                let hs = hit as usize;
                let (hx, hy, hz) = (dx*best, dy*best, dz*best);
                let (mut nx, mut ny, mut nz) = (hx-scx[hs], hy-scy[hs], hz-scz[hs]);
                let nl = (nx*nx+ny*ny+nz*nz).sqrt();
                nx /= nl; ny /= nl; nz /= nl;
                let (mut ldx, mut ldy, mut ldz) = (lx-hx, ly-hy, lz-hz);
                let ll = (ldx*ldx+ldy*ldy+ldz*ldz).sqrt();
                ldx /= ll; ldy /= ll; ldz /= ll;
                let mut diff = nx*ldx + ny*ldy + nz*ldz;
                if diff < 0.0 { diff = 0.0; }
                bright = (diff * 255.0) as i64;
                if bright > 255 { bright = 255; }
            }
            total += bright;
        }
    }
    println!("{}", total);
}
