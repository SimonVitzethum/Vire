// perft.rs — Rust-Parallelimplementierung des Chess-Perft-Benchmarks.
//
// Muss zu perft.cpp bitidentische stdout-Ausgabe liefern. Gleiche Datenstruktur
// (bb[8] als SoA), gleicher Algorithmus, keine Bibliotheken.
//
// Bauen: rustc -O -C target-cpu=native -o perft_rs perft.rs

type U64 = u64;

const WHITE: usize = 0;
const BLACK: usize = 1;
const PAWN: usize = 2;
const KNIGHT: usize = 3;
const BISHOP: usize = 4;
const ROOK: usize = 5;
const QUEEN: usize = 6;
const KING: usize = 7;

const CR_WK: u8 = 1;
const CR_WQ: u8 = 2;
const CR_BK: u8 = 4;
const CR_BQ: u8 = 8;

const MF_NORMAL: u32 = 0;
const MF_EP: u32 = 1;
const MF_CASTLE: u32 = 2;
const MF_DOUBLE: u32 = 3;

#[derive(Clone)]
struct Board {
    bb: [U64; 8],
    mailbox: [u8; 64],
    stm: usize,
    castle: u8,
    ep: i8,
    halfmove: u8,
}

#[derive(Clone, Copy, Default)]
struct Undo {
    captured: u8,
    castle: u8,
    ep: i8,
    halfmove: u8,
}

#[inline(always)]
fn mv_make(f: usize, t: usize, p: usize, fl: u32) -> u32 {
    (f as u32) | ((t as u32) << 6) | ((p as u32) << 12) | (fl << 15)
}
#[inline(always)]
fn mv_from(m: u32) -> usize { (m & 63) as usize }
#[inline(always)]
fn mv_to(m: u32) -> usize { ((m >> 6) & 63) as usize }
#[inline(always)]
fn mv_promo(m: u32) -> usize { ((m >> 12) & 7) as usize }
#[inline(always)]
fn mv_flag(m: u32) -> u32 { (m >> 15) & 3 }

#[inline(always)]
fn bit(sq: usize) -> U64 { 1u64 << sq }
#[inline(always)]
fn ctz64(b: U64) -> usize { b.trailing_zeros() as usize }
#[inline(always)]
fn pop_lsb(b: &mut U64) -> usize {
    let s = ctz64(*b);
    *b &= *b - 1;
    s
}
#[inline(always)]
fn file_of(sq: usize) -> usize { sq & 7 }
#[inline(always)]
fn rank_of(sq: usize) -> usize { sq >> 3 }

const KNIGHT_D: [[i32; 2]; 8] =
    [[1, 2], [2, 1], [2, -1], [1, -2], [-1, -2], [-2, -1], [-2, 1], [-1, 2]];
const KING_D: [[i32; 2]; 8] =
    [[0, 1], [1, 1], [1, 0], [1, -1], [0, -1], [-1, -1], [-1, 0], [-1, 1]];
const SLIDE_D: [[i32; 2]; 8] =
    [[0, 1], [0, -1], [1, 0], [-1, 0], [1, 1], [1, -1], [-1, 1], [-1, -1]];

struct Tables {
    knight: [U64; 64],
    king: [U64; 64],
    pawn: [[U64; 64]; 2],
}

fn init_tables() -> Tables {
    let mut t = Tables { knight: [0; 64], king: [0; 64], pawn: [[0; 64]; 2] };
    for sq in 0..64usize {
        let f = file_of(sq) as i32;
        let r = rank_of(sq) as i32;
        let mut n: U64 = 0;
        let mut k: U64 = 0;
        for i in 0..8 {
            let (nf, nr) = (f + KNIGHT_D[i][0], r + KNIGHT_D[i][1]);
            if nf >= 0 && nf < 8 && nr >= 0 && nr < 8 { n |= bit((nr * 8 + nf) as usize); }
            let (kf, kr) = (f + KING_D[i][0], r + KING_D[i][1]);
            if kf >= 0 && kf < 8 && kr >= 0 && kr < 8 { k |= bit((kr * 8 + kf) as usize); }
        }
        t.knight[sq] = n;
        t.king[sq] = k;

        let mut wp: U64 = 0;
        let mut bp: U64 = 0;
        if f > 0 && r < 7 { wp |= bit(sq + 7); }
        if f < 7 && r < 7 { wp |= bit(sq + 9); }
        if f > 0 && r > 0 { bp |= bit(sq - 9); }
        if f < 7 && r > 0 { bp |= bit(sq - 7); }
        t.pawn[WHITE][sq] = wp;
        t.pawn[BLACK][sq] = bp;
    }
    t
}

fn slide_attacks(sq: usize, occ: U64, d0: usize, d1: usize) -> U64 {
    let mut atk: U64 = 0;
    for d in d0..=d1 {
        let mut f = file_of(sq) as i32;
        let mut r = rank_of(sq) as i32;
        loop {
            f += SLIDE_D[d][0];
            r += SLIDE_D[d][1];
            if f < 0 || f > 7 || r < 0 || r > 7 { break; }
            let t = (r * 8 + f) as usize;
            atk |= bit(t);
            if occ & bit(t) != 0 { break; }
        }
    }
    atk
}
#[inline(always)]
fn rook_attacks(sq: usize, occ: U64) -> U64 { slide_attacks(sq, occ, 0, 3) }
#[inline(always)]
fn bishop_attacks(sq: usize, occ: U64) -> U64 { slide_attacks(sq, occ, 4, 7) }
#[inline(always)]
fn queen_attacks(sq: usize, occ: U64) -> U64 { slide_attacks(sq, occ, 0, 7) }

fn attacked(b: &Board, t: &Tables, sq: usize, by: usize) -> bool {
    let occ = b.bb[WHITE] | b.bb[BLACK];
    let them = b.bb[by];

    if t.pawn[by ^ 1][sq] & b.bb[PAWN] & them != 0 { return true; }
    if t.knight[sq] & b.bb[KNIGHT] & them != 0 { return true; }
    if t.king[sq] & b.bb[KING] & them != 0 { return true; }

    let bq = (b.bb[BISHOP] | b.bb[QUEEN]) & them;
    if bq != 0 && bishop_attacks(sq, occ) & bq != 0 { return true; }
    let rq = (b.bb[ROOK] | b.bb[QUEEN]) & them;
    if rq != 0 && rook_attacks(sq, occ) & rq != 0 { return true; }

    false
}

#[inline(always)]
fn put(b: &mut Board, sq: usize, color: usize, piece: usize) {
    b.bb[color] |= bit(sq);
    b.bb[piece] |= bit(sq);
    b.mailbox[sq] = piece as u8;
}
#[inline(always)]
fn clr(b: &mut Board, sq: usize, color: usize, piece: usize) {
    b.bb[color] &= !bit(sq);
    b.bb[piece] &= !bit(sq);
    b.mailbox[sq] = 0;
}

fn castle_mask(sq: usize) -> u8 {
    match sq {
        4 => CR_WK | CR_WQ,
        0 => CR_WQ,
        7 => CR_WK,
        60 => CR_BK | CR_BQ,
        56 => CR_BQ,
        63 => CR_BK,
        _ => 0,
    }
}

fn rook_squares(to: usize) -> (usize, usize) {
    match to {
        6 => (7, 5),
        2 => (0, 3),
        62 => (63, 61),
        _ => (56, 59),
    }
}

fn make_move(b: &mut Board, m: u32, u: &mut Undo) {
    let from = mv_from(m);
    let to = mv_to(m);
    let promo = mv_promo(m);
    let flag = mv_flag(m);
    let us = b.stm;
    let them = us ^ 1;
    let piece = b.mailbox[from] as usize;

    u.castle = b.castle;
    u.ep = b.ep;
    u.halfmove = b.halfmove;
    u.captured = 0;

    if flag == MF_EP {
        let capsq = if us == WHITE { to - 8 } else { to + 8 };
        u.captured = PAWN as u8;
        clr(b, capsq, them, PAWN);
    } else if b.mailbox[to] != 0 {
        u.captured = b.mailbox[to];
        clr(b, to, them, u.captured as usize);
    }

    clr(b, from, us, piece);
    put(b, to, us, if promo != 0 { promo } else { piece });

    if flag == MF_CASTLE {
        let (rf, rt) = rook_squares(to);
        clr(b, rf, us, ROOK);
        put(b, rt, us, ROOK);
    }

    b.castle &= !(castle_mask(from) | castle_mask(to));
    b.ep = if flag == MF_DOUBLE { ((from + to) / 2) as i8 } else { -1 };
    b.halfmove = if piece == PAWN || u.captured != 0 { 0 } else { b.halfmove.wrapping_add(1) };
    b.stm = them;
}

fn unmake_move(b: &mut Board, m: u32, u: &Undo) {
    let from = mv_from(m);
    let to = mv_to(m);
    let promo = mv_promo(m);
    let flag = mv_flag(m);
    let us = b.stm ^ 1;
    let them = b.stm;

    b.stm = us;
    b.castle = u.castle;
    b.ep = u.ep;
    b.halfmove = u.halfmove;

    let moved = if promo != 0 { PAWN } else { b.mailbox[to] as usize };
    clr(b, to, us, if promo != 0 { promo } else { moved });
    put(b, from, us, moved);

    if flag == MF_CASTLE {
        let (rf, rt) = rook_squares(to);
        clr(b, rt, us, ROOK);
        put(b, rf, us, ROOK);
    }

    if u.captured != 0 {
        if flag == MF_EP {
            let capsq = if us == WHITE { to - 8 } else { to + 8 };
            put(b, capsq, them, PAWN);
        } else {
            put(b, to, them, u.captured as usize);
        }
    }
}

fn gen_moves(b: &Board, t: &Tables, out: &mut [u32; 256]) -> usize {
    let mut n = 0usize;
    let us = b.stm;
    let them = us ^ 1;
    let own = b.bb[us];
    let opp = b.bb[them];
    let occ = own | opp;
    let empty = !occ;

    // Bauern
    let pawns = b.bb[PAWN] & own;
    let up: i32 = if us == WHITE { 8 } else { -8 };
    let rank3: U64 = if us == WHITE { 0x0000_0000_00FF_0000 } else { 0x0000_FF00_0000_0000 };
    let rank8: U64 = if us == WHITE { 0xFF00_0000_0000_0000 } else { 0x0000_0000_0000_00FF };

    let single = (if us == WHITE { pawns << 8 } else { pawns >> 8 }) & empty;
    let dbl = (if us == WHITE { (single & rank3) << 8 } else { (single & rank3) >> 8 }) & empty;

    let mut x = single;
    while x != 0 {
        let to = pop_lsb(&mut x);
        let from = (to as i32 - up) as usize;
        if bit(to) & rank8 != 0 {
            out[n] = mv_make(from, to, QUEEN, MF_NORMAL); n += 1;
            out[n] = mv_make(from, to, ROOK, MF_NORMAL); n += 1;
            out[n] = mv_make(from, to, BISHOP, MF_NORMAL); n += 1;
            out[n] = mv_make(from, to, KNIGHT, MF_NORMAL); n += 1;
        } else {
            out[n] = mv_make(from, to, 0, MF_NORMAL); n += 1;
        }
    }
    let mut x = dbl;
    while x != 0 {
        let to = pop_lsb(&mut x);
        let from = (to as i32 - 2 * up) as usize;
        out[n] = mv_make(from, to, 0, MF_DOUBLE); n += 1;
    }

    let mut p = pawns;
    while p != 0 {
        let from = pop_lsb(&mut p);
        let mut atk = t.pawn[us][from] & opp;
        while atk != 0 {
            let to = pop_lsb(&mut atk);
            if bit(to) & rank8 != 0 {
                out[n] = mv_make(from, to, QUEEN, MF_NORMAL); n += 1;
                out[n] = mv_make(from, to, ROOK, MF_NORMAL); n += 1;
                out[n] = mv_make(from, to, BISHOP, MF_NORMAL); n += 1;
                out[n] = mv_make(from, to, KNIGHT, MF_NORMAL); n += 1;
            } else {
                out[n] = mv_make(from, to, 0, MF_NORMAL); n += 1;
            }
        }
        if b.ep >= 0 && t.pawn[us][from] & bit(b.ep as usize) != 0 {
            out[n] = mv_make(from, b.ep as usize, 0, MF_EP); n += 1;
        }
    }

    // Springer
    let mut s = b.bb[KNIGHT] & own;
    while s != 0 {
        let from = pop_lsb(&mut s);
        let mut atk = t.knight[from] & !own;
        while atk != 0 { out[n] = mv_make(from, pop_lsb(&mut atk), 0, MF_NORMAL); n += 1; }
    }

    // Laeufer / Turm / Dame
    let mut s = b.bb[BISHOP] & own;
    while s != 0 {
        let from = pop_lsb(&mut s);
        let mut atk = bishop_attacks(from, occ) & !own;
        while atk != 0 { out[n] = mv_make(from, pop_lsb(&mut atk), 0, MF_NORMAL); n += 1; }
    }
    let mut s = b.bb[ROOK] & own;
    while s != 0 {
        let from = pop_lsb(&mut s);
        let mut atk = rook_attacks(from, occ) & !own;
        while atk != 0 { out[n] = mv_make(from, pop_lsb(&mut atk), 0, MF_NORMAL); n += 1; }
    }
    let mut s = b.bb[QUEEN] & own;
    while s != 0 {
        let from = pop_lsb(&mut s);
        let mut atk = queen_attacks(from, occ) & !own;
        while atk != 0 { out[n] = mv_make(from, pop_lsb(&mut atk), 0, MF_NORMAL); n += 1; }
    }

    // Koenig
    let ksq = ctz64(b.bb[KING] & own);
    let mut katk = t.king[ksq] & !own;
    while katk != 0 { out[n] = mv_make(ksq, pop_lsb(&mut katk), 0, MF_NORMAL); n += 1; }

    // Rochade
    if us == WHITE {
        if b.castle & CR_WK != 0 && occ & 0x60 == 0
            && !attacked(b, t, 4, BLACK) && !attacked(b, t, 5, BLACK) && !attacked(b, t, 6, BLACK) {
            out[n] = mv_make(4, 6, 0, MF_CASTLE); n += 1;
        }
        if b.castle & CR_WQ != 0 && occ & 0x0E == 0
            && !attacked(b, t, 4, BLACK) && !attacked(b, t, 3, BLACK) && !attacked(b, t, 2, BLACK) {
            out[n] = mv_make(4, 2, 0, MF_CASTLE); n += 1;
        }
    } else {
        if b.castle & CR_BK != 0 && occ & 0x6000_0000_0000_0000 == 0
            && !attacked(b, t, 60, WHITE) && !attacked(b, t, 61, WHITE) && !attacked(b, t, 62, WHITE) {
            out[n] = mv_make(60, 62, 0, MF_CASTLE); n += 1;
        }
        if b.castle & CR_BQ != 0 && occ & 0x0E00_0000_0000_0000 == 0
            && !attacked(b, t, 60, WHITE) && !attacked(b, t, 59, WHITE) && !attacked(b, t, 58, WHITE) {
            out[n] = mv_make(60, 58, 0, MF_CASTLE); n += 1;
        }
    }

    n
}

// Bewusst ohne Bulk-Counting, wie perft.cpp.
fn perft(b: &mut Board, t: &Tables, depth: i32) -> u64 {
    if depth == 0 { return 1; }

    let mut moves = [0u32; 256];
    let mut u = Undo::default();
    let n = gen_moves(b, t, &mut moves);
    let mut nodes = 0u64;
    let us = b.stm;

    for i in 0..n {
        make_move(b, moves[i], &mut u);
        let ksq = ctz64(b.bb[KING] & b.bb[us]);
        if !attacked(b, t, ksq, us ^ 1) {
            nodes += perft(b, t, depth - 1);
        }
        unmake_move(b, moves[i], &u);
    }
    nodes
}

fn piece_from_char(c: u8) -> (usize, usize) {
    match c {
        b'P' => (WHITE, PAWN),
        b'N' => (WHITE, KNIGHT),
        b'B' => (WHITE, BISHOP),
        b'R' => (WHITE, ROOK),
        b'Q' => (WHITE, QUEEN),
        b'K' => (WHITE, KING),
        b'p' => (BLACK, PAWN),
        b'n' => (BLACK, KNIGHT),
        b'b' => (BLACK, BISHOP),
        b'r' => (BLACK, ROOK),
        b'q' => (BLACK, QUEEN),
        b'k' => (BLACK, KING),
        _ => (0, 0),
    }
}

fn set_fen(fen: &str) -> Board {
    let mut b = Board { bb: [0; 8], mailbox: [0; 64], stm: WHITE, castle: 0, ep: -1, halfmove: 0 };
    let bytes = fen.as_bytes();
    let mut i = 0usize;
    let mut sq: i32 = 56;

    while i < bytes.len() && bytes[i] != b' ' {
        let c = bytes[i];
        if c == b'/' {
            sq -= 16;
        } else if c >= b'1' && c <= b'8' {
            sq += (c - b'0') as i32;
        } else {
            let (color, piece) = piece_from_char(c);
            if piece != 0 { put(&mut b, sq as usize, color, piece); sq += 1; }
        }
        i += 1;
    }
    while i < bytes.len() && bytes[i] == b' ' { i += 1; }
    if i < bytes.len() { b.stm = if bytes[i] == b'b' { BLACK } else { WHITE }; }
    while i < bytes.len() && bytes[i] != b' ' { i += 1; }
    while i < bytes.len() && bytes[i] == b' ' { i += 1; }
    while i < bytes.len() && bytes[i] != b' ' {
        match bytes[i] {
            b'K' => b.castle |= CR_WK,
            b'Q' => b.castle |= CR_WQ,
            b'k' => b.castle |= CR_BK,
            b'q' => b.castle |= CR_BQ,
            _ => {}
        }
        i += 1;
    }
    while i < bytes.len() && bytes[i] == b' ' { i += 1; }
    if i + 1 < bytes.len() && bytes[i] != b'-' {
        let f = (bytes[i] - b'a') as i32;
        let r = (bytes[i + 1] - b'1') as i32;
        if f >= 0 && f < 8 && r >= 0 && r < 8 { b.ep = (r * 8 + f) as i8; }
    }
    b
}

struct Case {
    name: &'static str,
    fen: &'static str,
    maxdepth: i32,
    expect: [u64; 8],
}

const CASES: [Case; 4] = [
    Case {
        name: "startpos",
        fen: "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        maxdepth: 7,
        expect: [1, 20, 400, 8902, 197281, 4865609, 119060324, 3195901860],
    },
    Case {
        name: "kiwipete",
        fen: "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        maxdepth: 5,
        expect: [1, 48, 2039, 97862, 4085603, 193690690, 0, 0],
    },
    Case {
        name: "position3",
        fen: "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        maxdepth: 7,
        expect: [1, 14, 191, 2812, 43238, 674624, 11030083, 178633661],
    },
    Case {
        name: "position4",
        fen: "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        maxdepth: 5,
        expect: [1, 6, 264, 9467, 422333, 15833292, 0, 0],
    },
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bench_depth: i32 = if args.len() > 1 { args[1].parse().unwrap_or(6) } else { 6 };
    let verify_max: i32 = if args.len() > 2 { args[2].parse().unwrap_or(5) } else { 5 };

    let t = init_tables();
    let mut failures = 0;

    for c in CASES.iter() {
        let dmax = if c.maxdepth < verify_max { c.maxdepth } else { verify_max };
        for d in 1..=dmax {
            let want = c.expect[d as usize];
            if want == 0 { continue; }
            let mut b = set_fen(c.fen);
            let got = perft(&mut b, &t, d);
            // stdout traegt nur die nackte Knotenzahl -- so ist sie zwischen
            // C++, Rust und Vire byteweise vergleichbar. Diagnose auf stderr.
            println!("{}", got);
            if got != want {
                eprintln!("FAIL {} depth {}: got {}, expected {}", c.name, d, got, want);
                failures += 1;
            }
        }
    }

    if failures > 0 {
        eprintln!("VERIFY: {} failures", failures);
        std::process::exit(1);
    }
    eprintln!("VERIFY: all ok");

    let mut b = set_fen(CASES[0].fen);
    let t0 = std::time::Instant::now();
    let nodes = perft(&mut b, &t, bench_depth);
    let dt = t0.elapsed().as_secs_f64();
    println!("{}", nodes);
    eprintln!("time {:.3} s   {:.2} Mnodes/s", dt, nodes as f64 / dt / 1e6);
}
