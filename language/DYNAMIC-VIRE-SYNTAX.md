# Vire Dynamic Surface — concrete syntax (proposal)

Companion to [DYNAMIC-VIRE-PLAN.md](DYNAMIC-VIRE-PLAN.md). This is the surface syntax for
open seams, effect contracts, plugin impls/overrides, module headers, loading, and
reflection. **Proposal only — nothing here is implemented.** Every construct maps to a
plan concept (§ refs point into the plan). Style follows existing Vire: `type`/`fn`/`impl`,
generics `[T]`, `@annotation`, `match … -> …`, `Result`/`?`.

The **golden rule**: a file with none of these keywords compiles and runs exactly as today
(sealed by default, §2). Dynamism is always visible in the source.

---

## 1. Declaring seams (host side)

A seam is the only place the closed-world optimizations are relaxed. Three forms:

```vire
// (a) an OPEN TRAIT — may gain new impls at runtime (§4.1)
open trait Renderer {
    fn render(self, ctx: DrawCtx)          // dispatched through a mutable slot
    fn z_order(self) -> Int = 0            // default body; overridable
}

// (b) an OPEN TYPE — participates in open traits; runtime code may impl traits for it
open type Entity { pos: Vec2  hp: Int }

// (c) a DYNAMIC FN — a standalone function redefinable/wrappable at runtime (§4.1)
dynamic fn on_tick(w: World) {
    // the default implementation (always present as the native slow path)
}
```

Sealed is the default and needs no keyword; `sealed`/`final` may be written for emphasis
but change nothing. A `trait` without `open` is closed-world: fully devirtualized, and a
runtime `impl` for it is **rejected at load** (the host never reserved a slot for it).

---

## 2. Effect contracts — buy back the optimizations a seam costs (§11.1)

Modifiers after `open`/`dynamic` declare a contract the **load verifier enforces on every
impl** (§6). They are what let the host keep RC-elision and the arena across a seam.

```vire
open trait Hook {
    // every impl the verifier accepts is guaranteed: does not let self/e escape the call,
    // performs no heap allocation, cannot panic. So the host may keep the ARENA firing
    // across on_event, and elide RC on its arguments.
    dynamic noescape noalloc nopanic fn on_event(self, e: Event) -> Bool
}

dynamic pure fn score(s: State) -> Int        // no observable effect, no retain → CSE-able
dynamic noescape fn transform(self, p: Path)  // p, self never escape → arena survives
```

| Modifier | Verifier guarantees every impl… | Host may therefore keep |
|---|---|---|
| `noescape` | never stores `self`/args/result past the call | **arena promotion** across the seam |
| `pure` | no side effect, no retain, deterministic | CSE / hoist / dead-call elimination |
| `noalloc` | performs no heap allocation | fixed-footprint hot loops |
| `nopanic` | cannot raise / abort | no unwind edges around the call |

An impl that violates its seam's contract **does not load** (it is a compile error on the
plugin side too, §7). No modifier ⇒ a *plain* seam: correct, but the arena/RC-elision stop
at it.

---

## 3. Providing impls & overrides (plugin side)

### 3.1 A new trait impl (the override capability)
```vire
use host.{ Entity, Renderer, DrawCtx }        // import the host's seam symbols

impl Renderer for Entity {                     // registered into the mutable slot on load
    fn render(self, ctx: DrawCtx) { ctx.sprite(self.pos, sprite_of(self)) }
    fn z_order(self) -> Int { self.pos.y as Int }
}
```

### 3.2 Redefining a `dynamic fn`
```vire
override fn on_tick(w: World) {                // replaces the host's on_tick pointer
    for e in w.entities { e.hp = e.hp - 1 }
}
```

### 3.3 Mixin-style around-advice — `prev` calls the implementation you replaced (§4.5)
```vire
override fn on_tick(w: World) {
    profile.begin("tick")
    prev(w)            // the previous slot value: the host default, or an earlier override
    profile.end("tick")
}
```
`prev` is the native pointer this override displaced — a real call, no bytecode weaving.
Chaining works: three mods each `override … { … prev(x) … }` form a native call chain in
load order, each able to run code before/after and to skip `prev` entirely to suppress.

---

## 4. Module headers — targeting, capabilities, weak edges

A plugin declares the host it was built against and the capabilities it needs. This is the
data the **load verifier** checks (§6) and the basis for the asymmetric-knowledge compile
(§2.1: the plugin is compiled *closed-world against `host@version`'s sealed surface*).

```vire
module hud {
    host game @ "1.2"        // pins host identity + ABI version it links against (§2.1)
    uses { ffi, threads }    // capability request — verifier rejects reaching beyond it (§11.4)
    weak-ok                  // opt-in: this module may introduce weak back-edges (§6)
}
```

Cross-cycle safety (the no-GC gate, §6): an owning field is default; a back-edge that would
close an ownership cycle **must** be `weak`, or the module is rejected:

```vire
open type Node {
    next: Node          // owning (RC)
    weak parent: Node   // non-owning — breaks the cycle so RC alone reclaims (no collector)
}
```

Building: `vire --emit-module hud.vr --link-against game.manifest` → `hud.so` + `hud.manifest`
(M1). The `.manifest` is generated, content-addressed, and carries: ABI version, host
identity+version, exported impls/overrides with their seam contracts, imported host
symbols, declared capabilities, and the module content hash (§11.6).

---

## 5. Loading & lifecycle (host runtime)

Loading is an ordinary typed call returning `Result` — a *checked link step*, never an
unsafe cast (§6). Failure is a value, not a crash.

```vire
fn main() {
    // M1: prebuilt native modules
    for path in mods_dir() {
        match load(path) {                       // verifier runs here; may reject
            Ok(m)  -> log.info("loaded {m.name} v{m.version}")
            Err(e) -> log.warn("skipped {path}: {e.error()}")   // e.g. ABI/contract/cycle
        }
    }
    // M2: compile-on-load from source (needs the vire toolchain at runtime)
    let plot = load_source("plot.vr")?           // subprocess-compiles + caches, then links

    // content-pinned load for reproducible composition (§11.6)
    let core = load_pinned("core", "sha256:9f2c…")?

    run_game()                                   // seam calls now hit loaded impls/overrides
}
```

Deterministic hot-swap & unload (RC, not GC → a defined reclamation point, §11.5):

```vire
let h = load("hud.so")?
swap(h, "hud_v2.so")?    // atomic at a safepoint; old instances drain under their old layout
unload(h)?               // drain instances + revert patches + free — at this defined point
```

Available capabilities intrinsics: `load`, `load_source`, `load_pinned`, `swap`, `unload`,
`mods_dir`/`mods` (host-defined discovery). All are ordinary functions; none exists unless
the program is built `--dynamic` (a sealed program can't accidentally gain a loader).

---

## 6. Reflection — compile-time descriptor, queried at runtime (§3)

No dynamic typing. `@typeinfo(T)` is a comptime-iterable view; `describe(x)` returns the
descriptor of a value's *runtime* type (which may be a loaded one), typed as `TypeInfo`.

```vire
// comptime: iterate a known type's fields (subsumes @derive)
comptime for f in @typeinfo(Entity).fields {
    print("{f.name}: {f.kind}")
}

// runtime: inspect a value whose concrete type may have been loaded at runtime
fn dump(x: Any) {                       // `Any` = a ref + its descriptor handle, not dynamic typing
    let d = describe(x)
    for f in d.fields { print("{f.name} @ {f.offset}") }
    for m in d.methods { print("method {m.name} slot {m.slot}") }
}
```

`TypeInfo`/`FieldInfo`/`MethodInfo` are ordinary Vire types the compiler populates; there is
no runtime type *creation*, only inspection of what was compiled.

---

## 7. End-to-end example (tiny host + tiny mod)

**Host (`game.vr`, built `vire --dynamic -o game`):**
```vire
open type Entity { pos: Vec2  hp: Int }

open trait Behavior {
    dynamic noescape fn update(self, e: Entity, dt: Float)   // noescape ⇒ arena kept (§11.1)
}

dynamic fn on_frame(w: World) {                              // overridable hook
    for e in w.entities { e.behavior.update(e, w.dt) }
}

fn main() {
    for p in mods() {
        match load(p) { Ok(m) -> log.info("mod {m.name}") Err(e) -> log.warn("{e.error()}") }
    }
    loop_frames(on_frame)
}
```

**Mod (`gravity.vr`, built `vire --emit-module gravity.vr --link-against game.manifest`):**
```vire
module gravity { host game @ "1" }

use game.{ Entity, Behavior }

struct Falling {}
impl Behavior for Falling {
    fn update(self, e: Entity, dt: Float) { e.pos.y = e.pos.y - 9.81 * dt }   // verified noescape
}

// also wrap the host frame hook (mixin-style): count frames, then run the original
override fn on_frame(w: World) { metrics.frames = metrics.frames + 1  prev(w) }
```

At load: the verifier checks `gravity.manifest` against `game`'s registry — ABI version,
that `Falling::update` honors the `noescape` contract, no cycle, capabilities within bounds.
Accepted → `Behavior` slot wired, `on_frame` pointer chained after the host default. First
`on_frame` call runs the mod natively; once the seam stabilizes, the JIT collapses it to a
guarded direct call (§5.1) — deterministically (§5.3), memory-safely (§6), no VM.

---

## 8. Grammar delta (informal)

```
item        := … | seam_item | override_item | module_header
seam_item   := "open" ("trait"|"type") …            // open trait / open type
             | vis? contract* "dynamic" "fn" sig block?   // dynamic fn (+ default body)
contract    := "noescape" | "pure" | "noalloc" | "nopanic"
trait_item  := contract* ("dynamic")? "fn" sig ("=" expr | block)?   // method + contracts
override_item := "override" "fn" sig block           // redefine a host dynamic fn
module_header := "module" ident "{" mod_decl* "}"
mod_decl    := "host" ident "@" str
             | "uses" "{" ident,* "}"
             | "weak-ok"
field       := "weak"? ident ":" type                // weak = non-owning back-edge
expr        := … | "prev" "(" args ")"               // the displaced implementation
```

New keywords: `open`, `dynamic`, `override`, `module`, `host`, `uses`, `weak`, `weak-ok`,
`prev`, plus the contract words `noescape`/`pure`/`noalloc`/`nopanic` (contextual — valid
only in seam position, so existing identifiers named e.g. `pure` still parse elsewhere).
`sealed`/`final` are accepted no-ops for emphasis.
