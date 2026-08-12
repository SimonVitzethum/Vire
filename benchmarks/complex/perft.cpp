/* perft.c — Referenzimplementierung des Chess-Perft-Benchmarks.
 *
 * Zweck: Spezifikation und Messreferenz fuer die parallelen Implementierungen
 * in Rust und Vire. Alle drei muessen *bitidentische* Ausgabe liefern; die
 * Knotenzahlen sind mathematisch feststehend (chessprogramming.org/Perft_Results,
 * verifiziert 2026-08-12), nicht "plausibel". Damit prueft dieser Benchmark als
 * einziger der Suite seine eigene Korrektheit exakt: ein falsch elidierter
 * Bounds-Check oder ein falsches noalias faellt sofort als falsche Zahl auf.
 *
 * Was er stresst:
 *   - SoA-Aliasing: bb[8] sind acht u64, die in make/unmake wechselseitig
 *     geschrieben und gelesen werden -- dieselbe Struktur wie NBody, nur in der
 *     innersten Schleife.
 *   - Bounds-Elision: mailbox[64], attack-Tabellen, Zuglisten move[256].
 *   - Tiefe Rekursion mit kleinem Frame (wie fib, nur mit Zustand).
 *   - Bitmanipulation: popcount, ctz, Shift/Mask in jeder Schleife.
 *
 * Version 1 bewusst ohne Magic Bitboards: Slider-Angriffe per Richtungsschleife.
 * Korrekt vor schnell -- diese Datei ist die Spezifikation fuer die anderen
 * Sprachen. Magic Bitboards kommen als v2 (stresst dann Speicher + nicht-affine
 * Indizes, wo die affine Bounds-Regel nicht greift).
 *
 * Bauen:  clang -O2 -march=native -o perft perft.c
 *         gcc   -O3 -march=native -o perft perft.c
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>

typedef uint64_t u64;
typedef uint32_t u32;
typedef uint8_t  u8;
typedef int8_t   i8;

/* ---- Indizes in Board.bb ------------------------------------------------ */
enum { WHITE = 0, BLACK = 1,
       PAWN  = 2, KNIGHT = 3, BISHOP = 4, ROOK = 5, QUEEN = 6, KING = 7 };

/* Rochaderechte */
enum { CR_WK = 1, CR_WQ = 2, CR_BK = 4, CR_BQ = 8 };

/* Zug-Flags */
enum { MF_NORMAL = 0, MF_EP = 1, MF_CASTLE = 2, MF_DOUBLE = 3 };

typedef struct {
    u64 bb[8];      /* SoA: [WHITE],[BLACK],[PAWN..KING] */
    u8  mailbox[64];/* Figurentyp je Feld, 0 = leer, sonst PAWN..KING */
    u8  stm;        /* WHITE oder BLACK */
    u8  castle;     /* CR_* Bitmaske */
    i8  ep;         /* En-passant-Zielfeld, -1 wenn keins */
    u8  halfmove;   /* 50-Zuege-Zaehler (fuer perft ohne Wirkung) */
} Board;

/* Zug: from(6) | to(6)<<6 | promo(3)<<12 | flag(2)<<15
 * promo: 0 = keine, sonst KNIGHT..QUEEN */
typedef u32 Move;
#define MV_MAKE(f,t,p,fl) ((u32)(f) | ((u32)(t)<<6) | ((u32)(p)<<12) | ((u32)(fl)<<15))
#define MV_FROM(m)  ((int)((m) & 63))
#define MV_TO(m)    ((int)(((m) >> 6) & 63))
#define MV_PROMO(m) ((int)(((m) >> 12) & 7))
#define MV_FLAG(m)  ((int)(((m) >> 15) & 3))

typedef struct {
    u8 captured;  /* geschlagener Figurentyp, 0 = keiner */
    u8 castle;
    i8 ep;
    u8 halfmove;
} Undo;

/* ---- Bit-Helfer --------------------------------------------------------- */
static inline int  ctz64(u64 b)      { return __builtin_ctzll(b); }
static inline u64  bit(int sq)       { return 1ULL << sq; }
static inline int  pop_lsb(u64 *b)   { int s = ctz64(*b); *b &= *b - 1; return s; }

#define FILE_OF(sq) ((sq) & 7)
#define RANK_OF(sq) ((sq) >> 3)

/* ---- Vorberechnete Tabellen --------------------------------------------- */
static u64 KNIGHT_ATK[64];
static u64 KING_ATK[64];
static u64 PAWN_ATK[2][64];

static const int KNIGHT_D[8][2] = {{1,2},{2,1},{2,-1},{1,-2},{-1,-2},{-2,-1},{-2,1},{-1,2}};
static const int KING_D[8][2]   = {{0,1},{1,1},{1,0},{1,-1},{0,-1},{-1,-1},{-1,0},{-1,1}};
/* Richtungen als (Datei, Reihe): 0-3 gerade (Turm), 4-7 diagonal (Laeufer) */
static const int SLIDE_D[8][2]  = {{0,1},{0,-1},{1,0},{-1,0},{1,1},{1,-1},{-1,1},{-1,-1}};

static void init_tables(void) {
    for (int sq = 0; sq < 64; sq++) {
        int f = FILE_OF(sq), r = RANK_OF(sq);
        u64 n = 0, k = 0;
        for (int i = 0; i < 8; i++) {
            int nf = f + KNIGHT_D[i][0], nr = r + KNIGHT_D[i][1];
            if (nf >= 0 && nf < 8 && nr >= 0 && nr < 8) n |= bit(nr * 8 + nf);
            nf = f + KING_D[i][0]; nr = r + KING_D[i][1];
            if (nf >= 0 && nf < 8 && nr >= 0 && nr < 8) k |= bit(nr * 8 + nf);
        }
        KNIGHT_ATK[sq] = n;
        KING_ATK[sq]   = k;

        u64 wp = 0, bp = 0;
        if (f > 0 && r < 7) wp |= bit(sq + 7);
        if (f < 7 && r < 7) wp |= bit(sq + 9);
        if (f > 0 && r > 0) bp |= bit(sq - 9);
        if (f < 7 && r > 0) bp |= bit(sq - 7);
        PAWN_ATK[WHITE][sq] = wp;
        PAWN_ATK[BLACK][sq] = bp;
    }
}

/* Slider-Angriffe per Richtungsschleife. v2: Magic Bitboards. */
static u64 slide_attacks(int sq, u64 occ, int d0, int d1) {
    u64 atk = 0;
    for (int d = d0; d <= d1; d++) {
        int f = FILE_OF(sq), r = RANK_OF(sq);
        for (;;) {
            f += SLIDE_D[d][0];
            r += SLIDE_D[d][1];
            if (f < 0 || f > 7 || r < 0 || r > 7) break;
            int t = r * 8 + f;
            atk |= bit(t);
            if (occ & bit(t)) break;
        }
    }
    return atk;
}
static inline u64 rook_attacks(int sq, u64 occ)   { return slide_attacks(sq, occ, 0, 3); }
static inline u64 bishop_attacks(int sq, u64 occ) { return slide_attacks(sq, occ, 4, 7); }
static inline u64 queen_attacks(int sq, u64 occ)  { return slide_attacks(sq, occ, 0, 7); }

/* ---- Angriffsabfrage ---------------------------------------------------- */
/* Wird Feld sq von Seite `by` angegriffen? */
static int attacked(const Board *b, int sq, int by) {
    u64 occ = b->bb[WHITE] | b->bb[BLACK];
    u64 them = b->bb[by];

    if (PAWN_ATK[by ^ 1][sq] & b->bb[PAWN]   & them) return 1;
    if (KNIGHT_ATK[sq]       & b->bb[KNIGHT] & them) return 1;
    if (KING_ATK[sq]         & b->bb[KING]   & them) return 1;

    u64 bq = (b->bb[BISHOP] | b->bb[QUEEN]) & them;
    if (bq && (bishop_attacks(sq, occ) & bq)) return 1;
    u64 rq = (b->bb[ROOK] | b->bb[QUEEN]) & them;
    if (rq && (rook_attacks(sq, occ) & rq)) return 1;

    return 0;
}

/* ---- make / unmake ------------------------------------------------------ */
static inline void put(Board *b, int sq, int color, int piece) {
    b->bb[color] |= bit(sq);
    b->bb[piece] |= bit(sq);
    b->mailbox[sq] = (u8)piece;
}
static inline void clr(Board *b, int sq, int color, int piece) {
    b->bb[color] &= ~bit(sq);
    b->bb[piece] &= ~bit(sq);
    b->mailbox[sq] = 0;
}

/* Rochaderechte, die beim Beruehren eines Feldes verfallen. */
static u8 castle_mask(int sq) {
    switch (sq) {
        case 4:  return (u8)(CR_WK | CR_WQ);  /* e1 */
        case 0:  return CR_WQ;                /* a1 */
        case 7:  return CR_WK;                /* h1 */
        case 60: return (u8)(CR_BK | CR_BQ);  /* e8 */
        case 56: return CR_BQ;                /* a8 */
        case 63: return CR_BK;                /* h8 */
        default: return 0;
    }
}

static void make_move(Board *b, Move m, Undo *u) {
    int from = MV_FROM(m), to = MV_TO(m);
    int promo = MV_PROMO(m), flag = MV_FLAG(m);
    int us = b->stm, them = us ^ 1;
    int piece = b->mailbox[from];

    u->castle   = b->castle;
    u->ep       = b->ep;
    u->halfmove = b->halfmove;
    u->captured = 0;

    if (flag == MF_EP) {
        int capsq = (us == WHITE) ? to - 8 : to + 8;
        u->captured = PAWN;
        clr(b, capsq, them, PAWN);
    } else if (b->mailbox[to]) {
        u->captured = b->mailbox[to];
        clr(b, to, them, u->captured);
    }

    clr(b, from, us, piece);
    put(b, to, us, promo ? promo : piece);

    if (flag == MF_CASTLE) {
        int rf, rt;
        if (to == 6)       { rf = 7;  rt = 5;  }   /* weiss kurz  */
        else if (to == 2)  { rf = 0;  rt = 3;  }   /* weiss lang  */
        else if (to == 62) { rf = 63; rt = 61; }   /* schwarz kurz*/
        else               { rf = 56; rt = 59; }   /* schwarz lang*/
        clr(b, rf, us, ROOK);
        put(b, rt, us, ROOK);
    }

    b->castle &= (u8)~(castle_mask(from) | castle_mask(to));
    b->ep = (flag == MF_DOUBLE) ? (i8)((from + to) / 2) : (i8)-1;
    b->halfmove = (piece == PAWN || u->captured) ? 0 : (u8)(b->halfmove + 1);
    b->stm = (u8)them;
}

static void unmake_move(Board *b, Move m, const Undo *u) {
    int from = MV_FROM(m), to = MV_TO(m);
    int promo = MV_PROMO(m), flag = MV_FLAG(m);
    int us = b->stm ^ 1, them = b->stm;

    b->stm     = (u8)us;
    b->castle  = u->castle;
    b->ep      = u->ep;
    b->halfmove= u->halfmove;

    int moved = promo ? PAWN : b->mailbox[to];
    clr(b, to, us, promo ? promo : moved);
    put(b, from, us, moved);

    if (flag == MF_CASTLE) {
        int rf, rt;
        if (to == 6)       { rf = 7;  rt = 5;  }
        else if (to == 2)  { rf = 0;  rt = 3;  }
        else if (to == 62) { rf = 63; rt = 61; }
        else               { rf = 56; rt = 59; }
        clr(b, rt, us, ROOK);
        put(b, rf, us, ROOK);
    }

    if (u->captured) {
        if (flag == MF_EP) {
            int capsq = (us == WHITE) ? to - 8 : to + 8;
            put(b, capsq, them, PAWN);
        } else {
            put(b, to, them, u->captured);
        }
    }
}

/* ---- Zuggenerierung (pseudo-legal) -------------------------------------- */
static int gen_moves(const Board *b, Move *out) {
    int n = 0;
    int us = b->stm, them = us ^ 1;
    u64 own = b->bb[us], opp = b->bb[them];
    u64 occ = own | opp;
    u64 empty = ~occ;

    /* --- Bauern --- */
    u64 pawns = b->bb[PAWN] & own;
    int up    = (us == WHITE) ? 8 : -8;
    u64 rank3 = (us == WHITE) ? 0x0000000000FF0000ULL : 0x0000FF0000000000ULL;
    u64 rank8 = (us == WHITE) ? 0xFF00000000000000ULL : 0x00000000000000FFULL;

    u64 single = (us == WHITE) ? (pawns << 8) : (pawns >> 8);
    single &= empty;
    u64 dbl = (us == WHITE) ? ((single & rank3) << 8) : ((single & rank3) >> 8);
    dbl &= empty;

    u64 t = single;
    while (t) {
        int to = pop_lsb(&t), from = to - up;
        if (bit(to) & rank8) {
            out[n++] = MV_MAKE(from, to, QUEEN,  MF_NORMAL);
            out[n++] = MV_MAKE(from, to, ROOK,   MF_NORMAL);
            out[n++] = MV_MAKE(from, to, BISHOP, MF_NORMAL);
            out[n++] = MV_MAKE(from, to, KNIGHT, MF_NORMAL);
        } else {
            out[n++] = MV_MAKE(from, to, 0, MF_NORMAL);
        }
    }
    t = dbl;
    while (t) { int to = pop_lsb(&t); out[n++] = MV_MAKE(to - 2*up, to, 0, MF_DOUBLE); }

    u64 p = pawns;
    while (p) {
        int from = pop_lsb(&p);
        u64 atk = PAWN_ATK[us][from] & opp;
        while (atk) {
            int to = pop_lsb(&atk);
            if (bit(to) & rank8) {
                out[n++] = MV_MAKE(from, to, QUEEN,  MF_NORMAL);
                out[n++] = MV_MAKE(from, to, ROOK,   MF_NORMAL);
                out[n++] = MV_MAKE(from, to, BISHOP, MF_NORMAL);
                out[n++] = MV_MAKE(from, to, KNIGHT, MF_NORMAL);
            } else {
                out[n++] = MV_MAKE(from, to, 0, MF_NORMAL);
            }
        }
        if (b->ep >= 0 && (PAWN_ATK[us][from] & bit(b->ep)))
            out[n++] = MV_MAKE(from, b->ep, 0, MF_EP);
    }

    /* --- Springer --- */
    u64 s = b->bb[KNIGHT] & own;
    while (s) {
        int from = pop_lsb(&s);
        u64 atk = KNIGHT_ATK[from] & ~own;
        while (atk) out[n++] = MV_MAKE(from, pop_lsb(&atk), 0, MF_NORMAL);
    }

    /* --- Laeufer / Turm / Dame --- */
    s = b->bb[BISHOP] & own;
    while (s) {
        int from = pop_lsb(&s);
        u64 atk = bishop_attacks(from, occ) & ~own;
        while (atk) out[n++] = MV_MAKE(from, pop_lsb(&atk), 0, MF_NORMAL);
    }
    s = b->bb[ROOK] & own;
    while (s) {
        int from = pop_lsb(&s);
        u64 atk = rook_attacks(from, occ) & ~own;
        while (atk) out[n++] = MV_MAKE(from, pop_lsb(&atk), 0, MF_NORMAL);
    }
    s = b->bb[QUEEN] & own;
    while (s) {
        int from = pop_lsb(&s);
        u64 atk = queen_attacks(from, occ) & ~own;
        while (atk) out[n++] = MV_MAKE(from, pop_lsb(&atk), 0, MF_NORMAL);
    }

    /* --- Koenig --- */
    int ksq = ctz64(b->bb[KING] & own);
    u64 katk = KING_ATK[ksq] & ~own;
    while (katk) out[n++] = MV_MAKE(ksq, pop_lsb(&katk), 0, MF_NORMAL);

    /* --- Rochade: Rechte, freie Felder, Koenig nicht durch Schach --- */
    if (us == WHITE) {
        if ((b->castle & CR_WK) && !(occ & 0x60ULL) &&
            !attacked(b, 4, BLACK) && !attacked(b, 5, BLACK) && !attacked(b, 6, BLACK))
            out[n++] = MV_MAKE(4, 6, 0, MF_CASTLE);
        if ((b->castle & CR_WQ) && !(occ & 0x0EULL) &&
            !attacked(b, 4, BLACK) && !attacked(b, 3, BLACK) && !attacked(b, 2, BLACK))
            out[n++] = MV_MAKE(4, 2, 0, MF_CASTLE);
    } else {
        if ((b->castle & CR_BK) && !(occ & 0x6000000000000000ULL) &&
            !attacked(b, 60, WHITE) && !attacked(b, 61, WHITE) && !attacked(b, 62, WHITE))
            out[n++] = MV_MAKE(60, 62, 0, MF_CASTLE);
        if ((b->castle & CR_BQ) && !(occ & 0x0E00000000000000ULL) &&
            !attacked(b, 60, WHITE) && !attacked(b, 59, WHITE) && !attacked(b, 58, WHITE))
            out[n++] = MV_MAKE(60, 58, 0, MF_CASTLE);
    }

    return n;
}

/* ---- perft -------------------------------------------------------------- */
/* Bewusst ohne Bulk-Counting: make/unmake soll auf jeder Ebene voll laufen,
 * das ist der eigentliche Compiler-Stress. Die Zahlen sind identisch. */
static u64 perft(Board *b, int depth) {
    if (depth == 0) return 1;

    Move moves[256];
    Undo u;
    int n = gen_moves(b, moves);
    u64 nodes = 0;
    int us = b->stm;

    for (int i = 0; i < n; i++) {
        make_move(b, moves[i], &u);
        int ksq = ctz64(b->bb[KING] & b->bb[us]);
        if (!attacked(b, ksq, us ^ 1))
            nodes += perft(b, depth - 1);
        unmake_move(b, moves[i], &u);
    }
    return nodes;
}

/* ---- FEN ---------------------------------------------------------------- */
static int piece_from_char(char c, int *color) {
    switch (c) {
        case 'P': *color = WHITE; return PAWN;
        case 'N': *color = WHITE; return KNIGHT;
        case 'B': *color = WHITE; return BISHOP;
        case 'R': *color = WHITE; return ROOK;
        case 'Q': *color = WHITE; return QUEEN;
        case 'K': *color = WHITE; return KING;
        case 'p': *color = BLACK; return PAWN;
        case 'n': *color = BLACK; return KNIGHT;
        case 'b': *color = BLACK; return BISHOP;
        case 'r': *color = BLACK; return ROOK;
        case 'q': *color = BLACK; return QUEEN;
        case 'k': *color = BLACK; return KING;
        default:  return 0;
    }
}

static void set_fen(Board *b, const char *fen) {
    memset(b, 0, sizeof(*b));
    b->ep = -1;

    int sq = 56;                     /* a8 */
    const char *s = fen;
    for (; *s && *s != ' '; s++) {
        if (*s == '/') { sq -= 16; continue; }
        if (*s >= '1' && *s <= '8') { sq += *s - '0'; continue; }
        int color, piece = piece_from_char(*s, &color);
        if (piece) { put(b, sq, color, piece); sq++; }
    }
    while (*s == ' ') s++;
    b->stm = (*s == 'b') ? BLACK : WHITE;
    while (*s && *s != ' ') s++;
    while (*s == ' ') s++;
    for (; *s && *s != ' '; s++) {
        if (*s == 'K') b->castle |= CR_WK;
        if (*s == 'Q') b->castle |= CR_WQ;
        if (*s == 'k') b->castle |= CR_BK;
        if (*s == 'q') b->castle |= CR_BQ;
    }
    while (*s == ' ') s++;
    if (*s && *s != '-') {
        int f = s[0] - 'a', r = s[1] - '1';
        if (f >= 0 && f < 8 && r >= 0 && r < 8) b->ep = (i8)(r * 8 + f);
    }
}

/* ---- Testfaelle --------------------------------------------------------- */
/* Alle Zahlen: chessprogramming.org/Perft_Results, verifiziert 2026-08-12. */
typedef struct { const char *name, *fen; int maxdepth; u64 expect[8]; } Case;

static const Case CASES[] = {
    { "startpos", "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1", 7,
      { 1, 20, 400, 8902, 197281, 4865609, 119060324, 3195901860ULL } },
    { "kiwipete", "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1", 5,
      { 1, 48, 2039, 97862, 4085603, 193690690ULL, 0, 0 } },
    { "position3", "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", 7,
      { 1, 14, 191, 2812, 43238, 674624, 11030083, 178633661ULL } },
    { "position4", "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1", 5,
      { 1, 6, 264, 9467, 422333, 15833292ULL, 0, 0 } },
};
#define NCASES ((int)(sizeof(CASES) / sizeof(CASES[0])))

static double now_sec(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec * 1e-9;
}

/* Ausgabe ist der Vergleichsgegenstand zwischen den Sprachen und muss
 * bitidentisch sein -- daher keine Zeiten in den Verifikationszeilen. */
int main(int argc, char **argv) {
    int bench_depth = (argc > 1) ? atoi(argv[1]) : 6;
    int verify_max  = (argc > 2) ? atoi(argv[2]) : 5;
    init_tables();

    int failures = 0;
    Board b;

    for (int c = 0; c < NCASES; c++) {
        int dmax = CASES[c].maxdepth < verify_max ? CASES[c].maxdepth : verify_max;
        for (int d = 1; d <= dmax; d++) {
            if (CASES[c].expect[d] == 0) continue;
            set_fen(&b, CASES[c].fen);
            u64 got = perft(&b, d);
            u64 want = CASES[c].expect[d];
            /* stdout traegt nur die nackte Knotenzahl -- so ist sie zwischen
             * C++, Rust und Vire byteweise vergleichbar. Diagnose auf stderr. */
            printf("%llu\n", (unsigned long long)got);
            if (got != want) {
                fprintf(stderr, "FAIL %s depth %d: got %llu, expected %llu\n",
                        CASES[c].name, d, (unsigned long long)got,
                        (unsigned long long)want);
                failures++;
            }
        }
    }

    if (failures) {
        fprintf(stderr, "VERIFY: %d failures\n", failures);
        return 1;
    }
    fprintf(stderr, "VERIFY: all ok\n");

    /* Messlauf: Startstellung, feste Tiefe. Zeit auf stderr, damit stdout
     * zwischen den Sprachen bitidentisch bleibt. */
    set_fen(&b, CASES[0].fen);
    double t0 = now_sec();
    u64 nodes = perft(&b, bench_depth);
    double dt = now_sec() - t0;
    printf("%llu\n", (unsigned long long)nodes);
    fprintf(stderr, "time %.3f s   %.2f Mnodes/s\n", dt, (double)nodes / dt / 1e6);
    return 0;
}
