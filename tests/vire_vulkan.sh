#!/bin/sh
# Vire @vulkan suite (V2 + VS step 1: Vire is the shader language).
#
# Renders the triangle headless via a real Vulkan graphics pipeline
# (crates/driver/src/vk_runtime.c). vk_triangle() returns the centroid pixel packed
# as 0xRRGGBB, so a Vire program can check the color — including one produced by a
# Vire `@fragment fn` (compiled to SPIR-V via spirv-as; crates/backend/src/spirv.rs).
# Skips cleanly without a Vulkan runtime/device or spirv-as. See language/GPU-VULKAN.md.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

if ! ls /usr/lib/libvulkan.so* >/dev/null 2>&1 && ! ls /usr/lib/*/libvulkan.so* >/dev/null 2>&1; then
    echo "skip vire_vulkan (no libvulkan)"; exit 0
fi
command -v spirv-as >/dev/null 2>&1 || { echo "skip vire_vulkan (no spirv-as)"; exit 0; }
if command -v vulkaninfo >/dev/null 2>&1; then
    vulkaninfo --summary 2>/dev/null | grep -q deviceName || { echo "skip vire_vulkan (no Vulkan device)"; exit 0; }
fi

work="$(mktemp -d)"; pass=0; fail=0
case_() {
    name="$1"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name" >/dev/null 2>"$work/e"; then
        if grep -qi "vulkan\|spirv" "$work/e"; then echo "skip $name (env: $(head -1 "$work/e"))"; return; fi
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$("$work/$name" 2>/dev/null | grep -v '^\[' | head -1)"
    if [ "$out" = "1" ]; then echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (got '$out', want '1')"; fail=$((fail+1)); fi
}

# Default fragment color (no @fragment shader) → orange centroid (~255,102,25).
case_ default_color <<'EOF'
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 200 { if g > 60 { if g < 140 { if b < 80 { ok = 1 } } } }
    print(ok)
}
EOF

# A Vire @fragment shader sets the color → the triangle renders in THAT color.
# green vec4(0.2,0.8,0.3,1) → centroid ~ (51,204,76). Proves Vire drives the shader.
case_ vire_fragment_shader <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(0.2, 0.8, 0.3, 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 90 { if g > 150 { if b > 30 { if b < 120 { ok = 1 } } } }
    print(ok)
}
EOF

# A Vire @fragment with real arithmetic (not a constant): a binding + vector*scalar
# → OpCompositeConstruct/OpVectorTimesScalar in the emitted SPIR-V. (0.1,0.4,0.15,0.5)*2
# = (0.2,0.8,0.3,1.0) green. Proves shader *bodies* compile, not just constants.
case_ vire_fragment_compute <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut base = vec4(0.1, 0.4, 0.15, 0.5)
    base * 2.0
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 90 { if g > 150 { if b > 30 { if b < 120 { ok = 1 } } } }
    print(ok)
}
EOF

# Per-pixel computation: r = gl_FragCoord.x / 256 (a horizontal gradient). At the
# sampled centroid (x=128) r≈128 — a value derived from the pixel POSITION, not any
# constant in the shader (which only has 256.0, 0.8, 0.3). Proves fragment inputs.
case_ vire_fragment_fragcoord <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(frag_x() / 256.0, 0.8, 0.3, 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 118 { if r < 138 { if g > 190 { if b > 60 { if b < 90 { ok = 1 } } } } }
    print(ok)
}
EOF

# A Vire @vertex shader TRANSFORMS the geometry: shifting every corner x+3 moves the
# triangle off-screen, so the centroid becomes the dark clear color. Proves the
# vertex stage is Vire-authored (both stages: @vertex + @fragment here).
case_ vire_vertex_shader <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 { vec4(pos.x + 3.0, pos.y, 0.0, 1.0) }
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.2, 0.2, 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 40 { if g < 40 { if b < 40 { ok = 1 } } }   // off-screen → dark clear
    print(ok)
}
EOF

# Varyings: the @vertex stage writes a per-vertex color via `out_color(vec3)`, the
# @fragment reads the INTERPOLATED value via `in_color()`. Color = position + 0.5
# (blue held constant 0.15). At the sampled centroid the interpolated r≈128, g≈152 —
# g≠r proves the per-vertex value is interpolated across the triangle (a flat
# fragment color cannot do this), and b≈38 is the constant channel. The vertex→
# fragment Location-0 link is auto-derived. Both stages Vire-authored.
case_ vire_varying_color <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 {
    out_color(vec3(pos.x + 0.5, pos.y + 0.5, 0.15))
    vec4(pos.x, pos.y, 0.0, 1.0)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 108 { if r < 148 { if g > r { if g < 180 { if b > 20 { if b < 60 { ok = 1 } } } } } }
    print(ok)
}
EOF

# Vertex-buffer geometry: vk_mesh(verts) renders a triangle from Vire DATA (a flat
# [Float] of interleaved x,y), not the built-in corners. (1) The default corners as
# Vire data render identically to vk_triangle (green centroid) — proving the vertex
# buffer path. (2) Shifting every x by +3 moves the triangle off-screen, so the
# centroid becomes the dark clear color — proving the Vire data drives the geometry.
# The bridge to GPU-driven meshlets (per-vertex data now comes from Vire).
case_ vire_mesh_buffer <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(0.2, 0.8, 0.3, 1.0) }
fn main() {
    mut tri = [0.0, -0.6, 0.6, 0.6, -0.6, 0.6]
    mut a = vk_mesh(tri)
    mut ar = a / 65536
    mut ag = (a / 256) % 256
    mut ab = a % 256
    mut off = [3.0, -0.6, 3.6, 0.6, 2.4, 0.6]
    mut b = vk_mesh(off)
    mut br = b / 65536
    mut bg = (b / 256) % 256
    mut bb = b % 256
    mut ok = 0
    if ar < 90 { if ag > 150 { if ab > 30 { if ab < 120 {   // (1) Vire data == built-in
      if br < 40 { if bg < 40 { if bb < 40 {                 // (2) off-screen → clear
        ok = 1 } } } } } } }
    print(ok)
}
EOF

# Per-vertex color attributes: vk_mesh_c(verts) interleaves (x,y, r,g,b) per vertex.
# The @vertex reads its own color via attr_color() (vertex-buffer Location 1) and
# forwards it as a varying; the fragment paints the interpolated result. The classic
# RGB triangle: red/green/blue corners. At the centroid ALL THREE channels are
# present (each 40..160) — only possible if three pure per-vertex colors interpolate
# (a flat or position-derived color cannot). Typed stage I/O: geometry + attributes
# from Vire.
case_ vire_mesh_attr_color <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 {
    out_color(attr_color())
    vec4(pos.x, pos.y, 0.0, 1.0)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut tri = [0.0, -0.6, 1.0, 0.0, 0.0, 0.6, 0.6, 0.0, 1.0, 0.0, -0.6, 0.6, 0.0, 0.0, 1.0]
    mut px = vk_mesh_c(tri)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 40 { if r < 160 { if g > 40 { if g < 160 { if b > 40 { if b < 160 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# Structured control flow — BRANCH: the fragment picks its color with an `if` on the
# pixel position (OpSelectionMerge). The centroid (x=128) is >= 100, so it takes the
# else branch → blue. A per-pixel decision, not a constant.
case_ vire_shader_branch <<'EOF'
@fragment
fn fs() -> Vec4 {
    if frag_x() < 100.0 { vec4(0.9, 0.1, 0.1, 1.0) } else { vec4(0.1, 0.1, 0.9, 1.0) }
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 60 { if g < 60 { if b > 200 { ok = 1 } } }   // else branch → blue
    print(ok)
}
EOF

# Structured control flow — LOOP: a `while` accumulates 0.1 five times (OpLoopMerge)
# into the red channel → 0.5 → ~128. Proves real iteration with a mutable local
# carried across the loop back-edge (the storage-variable model).
case_ vire_shader_loop <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut acc = 0.0
    mut i = 0.0
    while i < 5.0 {
        acc = acc + 0.1
        i = i + 1.0
    }
    vec4(acc, 0.2, 0.7, 1.0)
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 118 { if r < 138 { if g > 40 { if g < 64 { if b > 165 { if b < 190 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# GLSL.std.450 builtins: a Lambert term from normalize()/dot()/max(), then mix()
# between a dark and a bright color. dot(normalize(.3,.4,1), (0,0,1)) = 0.894, so
# mix gives centroid ~ (47,207,70). Proves real vector math (OpExtInst + OpDot), the
# lighting primitives shaders need.
case_ vire_shader_glsl <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut nrm = normalize(vec3(0.3, 0.4, 1.0))
    mut lgt = normalize(vec3(0.0, 0.0, 1.0))
    mut d = max(dot(nrm, lgt), 0.0)
    mut base = mix(vec3(0.05, 0.05, 0.05), vec3(0.2, 0.9, 0.3), d)
    vec4(base, 1.0)
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 40 { if r < 55 { if g > 195 { if g < 216 { if b > 62 { if b < 78 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# GPU-driven mesh shader (VM milestone): vk_mesh_shader() draws via a mesh pipeline
# (VK_EXT_mesh_shader / vkCmdDrawMeshTasksEXT) — the mesh shader emits the triangle
# itself, no vertex buffer and no vertex stage. The Vire @fragment colors it orange
# (0.9,0.5,0.1) → centroid ~ (229,127,25). Returns -2 where the device lacks mesh
# shaders, so the case passes (as a skip) there too.
case_ vire_mesh_shader <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.5, 0.1, 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }                          // no mesh-shader device → skip-pass
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r > 200 { if g > 105 { if g < 150 { if b < 60 { ok = 1 } } } }
    }
    print(ok)
}
EOF

# Vire-authored @mesh + @task (amplification): all THREE stages come from Vire. The
# @task emits one mesh workgroup; the @mesh computes the triangle's positions (note
# `mut s` + arithmetic) and writes the connectivity; the @fragment colors it cyan
# (0.2,0.7,0.9) → centroid ~ (51,178,229). -2 → skip where no mesh-shader device.
case_ vire_mesh_authored <<'EOF'
@task
fn ts() { emit_mesh_tasks(1) }
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut s = 0.6
    mesh_pos(0, vec4(0.0, 0.0 - s, 0.0, 1.0))
    mesh_pos(1, vec4(s, s, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - s, s, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.2, 0.7, 0.9, 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r < 65 { if g > 165 { if g < 190 { if b > 215 { ok = 1 } } } }
    }
    print(ok)
}
EOF

# The amplification shader CULLS: emit_mesh_tasks(0) launches no meshlet, so the
# triangle never renders and the centroid stays the dark clear color (~20,20,25).
# Proves the @task stage gates GPU geometry — the basis for GPU-driven culling.
case_ vire_task_cull <<'EOF'
@task
fn ts() { emit_mesh_tasks(0) }
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mesh_pos(0, vec4(0.0, 0.0 - 0.6, 0.0, 1.0))
    mesh_pos(1, vec4(0.6, 0.6, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - 0.6, 0.6, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.2, 0.2, 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r < 40 { if g < 40 { if b < 40 { ok = 1 } } }   // culled → clear color
    }
    print(ok)
}
EOF

# GPU frustum culling in the @task shader: the host pushes a frustum plane
# (cull_plane()), the task shader tests the meshlet's bounding-sphere center on the
# GPU (dot + compare → OpSelect emit 1/0). The SAME meshlet renders (plane (0,0,1,0)
# → d=0, visible) or is culled (plane (0,0,1,-2) → d=-2, behind) purely from the
# camera data — the basis for GPU-driven culling. -2 → skip where no mesh device.
case_ vire_task_gpu_cull <<'EOF'
@task
fn ts() {
    mut plane = cull_plane()
    mut center = vec4(0.0, 0.0, 0.0, 1.0)
    mut d = dot(plane, center)
    emit_mesh_tasks(d > 0.0 - 0.6)
}
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mesh_pos(0, vec4(0.0, 0.0 - 0.6, 0.0, 1.0))
    mesh_pos(1, vec4(0.6, 0.6, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - 0.6, 0.6, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.6, 0.1, 1.0) }
fn main() {
    mut vis = vk_mesh_shader(0.0, 0.0, 1.0, 0.0)
    mut cul = vk_mesh_shader(0.0, 0.0, 1.0, 0.0 - 2.0)
    mut ok = 0
    if vis == -2 { ok = 1 }
    if vis > 0 {
        if vis / 65536 > 200 {          // visible → orange
            if cul / 65536 < 40 { ok = 1 }   // culled → dark clear
        }
    }
    print(ok)
}
EOF

# Many meshlets from a Vire scene buffer, one indirect dispatch. The [Float] of
# per-meshlet (x,y) offsets is uploaded to an SSBO; N mesh workgroups are dispatched
# via vkCmdDrawMeshTasksIndirectEXT, and each @mesh workgroup reads its own offset
# with meshlet_offset() (scene[gl_WorkGroupID.x]). Two meshlets — left (x=-0.5) and
# right (x=+0.5) — both render → mask 3. Data-driven: the scene array decides the
# geometry. -2 → skip where no mesh device.
case_ vire_mesh_scene <<'EOF'
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut o = meshlet_offset()
    mesh_pos(0, vec4(o.x, o.y - 0.15, 0.0, 1.0))
    mesh_pos(1, vec4(o.x + 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_pos(2, vec4(o.x - 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.3, 0.8, 0.4, 1.0) }
fn main() {
    mut scene = [0.0 - 0.5, 0.0, 0.5, 0.0]
    mut mask = vk_mesh_scene(scene)
    mut ok = 0
    if mask == -2 { ok = 1 }
    if mask == 3 { ok = 1 }        // both left and right meshlets rendered
    print(ok)
}
EOF

# Fused GPU-driven cull renderer: scene buffer + per-meshlet culling in one dispatch.
# N task workgroups each read their meshlet's center (meshlet_offset), test it against
# the pushed frustum plane, and emit ONLY the survivors — the payload carries the
# meshlet index to the mesh workgroup, which reads scene[payload.idx] (culled_offset).
# With plane d=1 both meshlets pass (mask 3); with plane x>0 the left one (x=-0.5) is
# culled on the GPU (mask 2). -2 → skip where no mesh device.
case_ vire_scene_cull <<'EOF'
@task
fn ts() {
    mut o = meshlet_offset()
    mut plane = cull_plane()
    mut d = dot(plane, vec4(o.x, o.y, 0.0, 1.0))
    emit_visible(d > 0.0 - 0.2)
}
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut o = culled_offset()
    mesh_pos(0, vec4(o.x, o.y - 0.15, 0.0, 1.0))
    mesh_pos(1, vec4(o.x + 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_pos(2, vec4(o.x - 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.4, 0.7, 0.9, 1.0) }
fn main() {
    mut scene = [0.0 - 0.5, 0.0, 0.5, 0.0]
    mut both = vk_mesh_scene_cull(scene, 0.0, 0.0, 0.0, 1.0)
    mut only = vk_mesh_scene_cull(scene, 1.0, 0.0, 0.0, 0.0)
    mut ok = 0
    if both == -2 { ok = 1 }
    if both == 3 { if only == 2 { ok = 1 } }   // both pass, then left culled on GPU
    print(ok)
}
EOF

# Fully GPU-built renderer: a @compute builder fills the scene SSBO on the GPU
# (set_meshlet, indexed by meshlet_index()), a barrier, then the @task cull + @mesh
# draw run over it — the meshlet set never exists on the host. Two built meshlets
# (left x=-0.5, right x=+0.5): permissive plane shows both (mask 3), +x plane culls
# the left on the GPU (mask 2). Every stage — build, cull, emit, shade — is Vire.
case_ vire_mesh_built <<'EOF'
@compute
fn build() {
    mut x = 0.0 - 0.5 + meshlet_index() * 1.0
    set_meshlet(vec2(x, 0.0))
}
@task
fn ts() {
    mut o = meshlet_offset()
    mut plane = cull_plane()
    mut d = dot(plane, vec4(o.x, o.y, 0.0, 1.0))
    emit_visible(d > 0.0 - 0.2)
}
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut o = culled_offset()
    mesh_pos(0, vec4(o.x, o.y - 0.15, 0.0, 1.0))
    mesh_pos(1, vec4(o.x + 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_pos(2, vec4(o.x - 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.5, 0.8, 0.3, 1.0) }
fn main() {
    mut both = vk_mesh_built(2, 0.0, 0.0, 0.0, 1.0)
    mut only = vk_mesh_built(2, 1.0, 0.0, 0.0, 0.0)
    mut ok = 0
    if both == -2 { ok = 1 }
    if both == 3 { if only == 2 { ok = 1 } }
    print(ok)
}
EOF

# Typed scene records + cone/backface culling. The scene record is a Vire struct
# Meshlet { offset: vec2, cone: vec2 }; the @compute builder writes BOTH fields
# (set_meshlet(offset, cone)) and the @task reads the facing direction (meshlet_cone)
# to backface-cull. Meshlet 0 faces toward (cone.x=+1) → drawn; meshlet 1 faces away
# (cone.x=-1) → culled on the GPU. Mask 1 = only the left survived. -2 → skip.
case_ vire_cone_cull <<'EOF'
@compute
fn build() {
    mut x = 0.0 - 0.5 + meshlet_index() * 1.0
    mut facing = 1.0 - meshlet_index() * 2.0
    set_meshlet(vec2(x, 0.0), vec2(facing, 0.0))
}
@task
fn ts() {
    mut cone = meshlet_cone()
    emit_visible(cone.x > 0.0)
}
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut o = culled_offset()
    mesh_pos(0, vec4(o.x, o.y - 0.15, 0.0, 1.0))
    mesh_pos(1, vec4(o.x + 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_pos(2, vec4(o.x - 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.8, 0.5, 0.9, 1.0) }
fn main() {
    mut mask = vk_mesh_built(2, 0.0, 0.0, 0.0, 1.0)
    mut ok = 0
    if mask == -2 { ok = 1 }
    if mask == 1 { ok = 1 }        // left (facing toward) survives; right (away) culled
    print(ok)
}
EOF

# Per-vertex mesh-shader attributes → fragment. The @mesh writes a per-vertex colour
# (mesh_color(i, vec3)) at Location 0; the @fragment reads it interpolated via
# in_color() — the classic RGB triangle, but the colours come from the MESH shader
# (not a vertex buffer). At the centroid all three channels blend (each 40..160),
# only possible if three pure per-vertex colours interpolate. -2 → skip.
case_ vire_mesh_color <<'EOF'
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mesh_pos(0, vec4(0.0, 0.0 - 0.6, 0.0, 1.0))
    mesh_pos(1, vec4(0.6, 0.6, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - 0.6, 0.6, 0.0, 1.0))
    mesh_color(0, vec3(1.0, 0.0, 0.0))
    mesh_color(1, vec3(0.0, 1.0, 0.0))
    mesh_color(2, vec3(0.0, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r > 40 { if r < 160 { if g > 40 { if g < 160 { if b > 40 { if b < 160 {
            ok = 1 } } } } } }
    }
    print(ok)
}
EOF

# Multi-component swizzles (.rgb/.xy → OpVectorShuffle) + `if` as a statement (effect-
# only branches → OpSelectionMerge). tint=0.5 (xy.x=0.2<0.3) added to r: r=(0.2+0.5)*255
# =178, g=0.4*255=102, b=0.6*255=153.
case_ vire_swizzle_ifstmt <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut base = vec4(0.2, 0.4, 0.6, 1.0)
    mut rgb = base.rgb
    mut xy = base.xy
    mut tint = 0.0
    if xy.x < 0.3 { tint = 0.5 } else { tint = 0.1 }
    vec4(rgb.r + tint, rgb.g, rgb.b, 1.0)
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 168 { if r < 188 { if g > 92 { if g < 112 { if b > 143 { if b < 163 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# @gpuvk — vendor-neutral Vulkan compute (any device, not just mesh-shader ones).
# A data-parallel map runs on the GPU over a Float array in place: each element x
# becomes x*3 + 1. gpuvk_run returns 0 (ok) or -2 (no device → skip). Check a[2]:
# 30*3+1 = 91, and a[0]: 10*3+1 = 31.
case_ vire_gpuvk_compute <<'EOF'
@gpuvk
fn f() -> Float {
    mut x = elem()
    x * 3.0 + 1.0
}
fn main() {
    mut a = [10.0, 20.0, 30.0, 40.0]
    mut st = gpuvk_run(a)
    mut ok = 0
    if st == -2 { ok = 1 }
    if st == 0 {
        mut a0 = a[0]
        mut a2 = a[2]
        if a0 > 30.0 { if a0 < 32.0 { if a2 > 90.0 { if a2 < 92.0 { ok = 1 } } } }
    }
    print(ok)
}
EOF

# Host uniform (push constant): the fragment reads uniform() (a vec4 the host sets via
# vk_triangle(r,g,b,_)) → the triangle renders in that host-controlled colour. Proves
# host→shader parameters. Centroid (229,76,153) for (0.9,0.3,0.6).
case_ vire_uniform <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut u = uniform()
    vec4(u.x, u.y, u.z, 1.0)
}
fn main() {
    mut px = vk_triangle(0.9, 0.3, 0.6, 0.0)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 219 { if g > 66 { if g < 86 { if b > 143 { if b < 163 { ok = 1 } } } } }
    print(ok)
}
EOF

# Host-driven vertex transform: the @vertex reads uniform() and transforms the
# geometry. A +3 x-shift (uniform.x) moves the triangle off-screen → centroid = dark
# clear (~20); no shift → visible red (~229). Proves uniform() in the vertex stage.
case_ vire_vertex_uniform <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 {
    mut u = uniform()
    vec4(pos.x + u.x, pos.y * u.y, 0.0, 1.0)
}
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.2, 0.2, 1.0) }
fn main() {
    mut off = vk_triangle(3.0, 1.0, 0.0, 0.0)
    mut on = vk_triangle(0.0, 1.0, 0.0, 0.0)
    mut ok = 0
    if off / 65536 < 40 { if on / 65536 > 200 { ok = 1 } }
    print(ok)
}
EOF

# Wider scene records: the Meshlet struct now carries a per-record colour
# (offset:vec2, cone:vec2, color:vec4). The @compute builder writes it with
# set_meshlet_color; the @mesh reads it with meshlet_rgb and forwards it per vertex
# (mesh_color); the fragment paints it. A GPU-built red meshlet (0.9,0.1,0.15) at the
# left → left pixel ~ (229,25,38). vk_built_color returns that pixel. -2 → skip.
case_ vire_meshlet_color <<'EOF'
@compute
fn build() {
    set_meshlet(vec2(0.0 - 0.5, 0.0), vec2(1.0, 0.0))
    set_meshlet_color(vec3(0.9, 0.1, 0.15))
}
@task
fn ts() {
    mut cone = meshlet_cone()
    emit_visible(cone.x > 0.0)
}
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut o = culled_offset()
    mut c = meshlet_rgb()
    mesh_pos(0, vec4(o.x, o.y - 0.15, 0.0, 1.0))
    mesh_pos(1, vec4(o.x + 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_pos(2, vec4(o.x - 0.15, o.y + 0.15, 0.0, 1.0))
    mesh_color(0, c)
    mesh_color(1, c)
    mesh_color(2, c)
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut px = vk_built_color(1, 0.0, 0.0, 0.0, 1.0)
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        if r > 200 { if g < 60 { ok = 1 } }
    }
    print(ok)
}
EOF

# Depth buffer: a @mesh emits two overlapping triangles — a red one at z=0.2 (drawn
# FIRST) and a blue one at z=0.8 (drawn SECOND). With depth testing the front (red)
# wins at the centroid despite the blue being drawn later; without depth the blue
# would overwrite. Centroid red (r~229, b~25) proves depth occlusion. -2 → skip.
case_ vire_depth <<'EOF'
@mesh
fn ms() {
    set_mesh_outputs(6, 2)
    mesh_pos(0, vec4(0.0, 0.0 - 0.6, 0.2, 1.0))
    mesh_pos(1, vec4(0.6, 0.6, 0.2, 1.0))
    mesh_pos(2, vec4(0.0 - 0.6, 0.6, 0.2, 1.0))
    mesh_color(0, vec3(0.9, 0.1, 0.1))
    mesh_color(1, vec3(0.9, 0.1, 0.1))
    mesh_color(2, vec3(0.9, 0.1, 0.1))
    mesh_tri(0, 0, 1, 2)
    mesh_pos(3, vec4(0.0, 0.0 - 0.6, 0.8, 1.0))
    mesh_pos(4, vec4(0.6, 0.6, 0.8, 1.0))
    mesh_pos(5, vec4(0.0 - 0.6, 0.6, 0.8, 1.0))
    mesh_color(3, vec3(0.1, 0.1, 0.9))
    mesh_color(4, vec3(0.1, 0.1, 0.9))
    mesh_color(5, vec3(0.1, 0.1, 0.9))
    mesh_tri(1, 3, 4, 5)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 { if px / 65536 > 200 { if px % 256 < 60 { ok = 1 } } }   // red front wins
    print(ok)
}
EOF

# Textures: the fragment samples a 2x2 RGBA texture (red/green/blue/orange quadrants)
# with tex(uv) — a combined image sampler at set 0 binding 0, uv from gl_FragCoord.
# The centroid (uv=0.5,0.55, NEAREST) samples the orange texel -> (255,128,0).
case_ vire_texture <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut uv = vec2(frag_x() / 256.0, frag_y() / 256.0)
    tex(uv)
}
fn main() {
    mut px = vk_textured()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 240 { if g > 110 { if g < 145 { if b < 30 { ok = 1 } } } }   // orange texel
    print(ok)
}
EOF

# Render graph (first step): two passes with an automatic layout transition. Pass 1
# renders a fixed-red triangle into an offscreen texture; the runtime auto-transitions
# it COLOR_ATTACHMENT -> SHADER_READ_ONLY (auto_barrier derives the barrier); pass 2
# samples it with the program's tex(uv) @fragment. Centroid red (~229,51,51) proves
# the first-pass output is correctly barriered and sampled in the second pass.
case_ vire_two_pass <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut uv = vec2(frag_x() / 256.0, frag_y() / 256.0)
    tex(uv)
}
fn main() {
    mut px = vk_two_pass()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 200 { if g < 80 { ok = 1 } }
    print(ok)
}
EOF

# Texture as a first-class Vire value (typed-handle first step): vk_texture_draw(pix, w)
# builds a GPU texture from an RC-managed [Float] of RGBA and renders sampling it — the
# handle (the Vire array) is lifetime-safe by construction (no GPU resource outlives the
# call). A 2x2 texture whose texel(1,1)=(0.2,0.8,0.4) → centroid (51,204,102).
case_ vire_texture_value <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut uv = vec2(frag_x() / 256.0, frag_y() / 256.0)
    tex(uv)
}
fn main() {
    mut pix = [1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.2, 0.8, 0.4, 1.0]
    mut px = vk_texture_draw(pix, 2)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 70 { if g > 185 { if b > 85 { if b < 120 { ok = 1 } } } }
    print(ok)
}
EOF

# RC-bound GPU texture handle (real lifetime safety): vk_texture_new(pix, w) creates a
# PERSISTENT GPU texture in a persistent Vulkan context and returns a Vire object whose
# drop frees it. vk_draw_handle(t) draws with it WITHOUT re-uploading. When t drops, the
# runtime's RC destroys the GPU texture (custom vtable drop) — verified 0-live below.
# Two draws sample the same texel(1,1)=(0.2,0.8,0.4) → green (51,204).
case_ vire_texture_handle <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut uv = vec2(frag_x() / 256.0, frag_y() / 256.0)
    tex(uv)
}
fn main() {
    mut pix = [1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.2, 0.8, 0.4, 1.0]
    mut t = vk_texture_new(pix, 2)
    mut c1 = vk_draw_handle(t)
    mut c2 = vk_draw_handle(t)
    mut ok = 0
    if (c1 / 256) % 256 > 185 { if (c2 / 256) % 256 > 185 { ok = 1 } }
    print(ok)
}
EOF

# Render graph, deepened: vk_chain(n) is an N-pass chain — pass 0 renders red into a
# texture, each pass i samples texture[i-1] into texture[i], a final copy reads the
# last. The runtime TRACKS each texture's layout and auto-inserts the barrier at every
# hop (not a fixed 2-pass). A 3-pass chain propagates the red → centroid (~229,51). A
# missing/wrong barrier would give undefined data or a validation error, not red.
case_ vire_chain <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut uv = vec2(frag_x() / 256.0, frag_y() / 256.0)
    tex(uv)
}
fn main() {
    mut px = vk_chain(3)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 200 { if g < 80 { ok = 1 } }
    print(ok)
}
EOF

# RC-bound GPU BUFFER handle (typed handles generalize beyond textures): vk_buffer_new
# uploads a Vire [Float] to a persistent GPU storage buffer and returns a Vire object
# whose drop frees it; vk_buffer_get(b, i) reads element i with no re-upload. When b
# drops the runtime frees the GPU buffer (verified 0-live). data[2]=30, data[4]=50.
case_ vire_buffer_handle <<'EOF'
fn main() {
    mut data = [10.0, 20.0, 30.0, 40.0, 50.0]
    mut b = vk_buffer_new(data)
    mut x2 = vk_buffer_get(b, 2)
    mut x4 = vk_buffer_get(b, 4)
    mut ok = 0
    if x2 > 29.0 { if x2 < 31.0 { if x4 > 49.0 { if x4 < 51.0 { ok = 1 } } } }
    print(ok)
}
EOF

# Render graph with a MULTI-INPUT pass (a DAG, not a chain): two source passes render
# red -> A and blue -> B, then a blend pass samples BOTH (tex(uv)+tex2(uv), mix 0.5).
# The runtime auto-transitions BOTH inputs to SHADER_READ_ONLY before the fan-in pass.
# Blend of red(0.9,0.2,0.2)+blue(0.1,0.2,0.9) -> (127,51,140).
case_ vire_blend2 <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut uv = vec2(frag_x() / 256.0, frag_y() / 256.0)
    mix(tex(uv), tex2(uv), 0.5)
}
fn main() {
    mut px = vk_blend2()
    mut r = px / 65536
    mut b = px % 256
    mut ok = 0
    if r > 115 { if r < 140 { if b > 125 { if b < 155 { ok = 1 } } } }
    print(ok)
}
EOF

# Persistent render session + per-frame Vire-driven rendering (interactive core, RC-bound):
# vk_session() creates a persistent target+pipeline+buffers ONCE (a third RC handle type);
# a Vire while-loop calls vk_frame(s, r,g,b,a) each frame with an animating uniform (no
# per-frame setup). The last frame (r=0.8) reads (204,76,128). The session's GPU objects
# are freed when s drops (verified 0-live elsewhere).
case_ vire_session <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut u = uniform()
    vec4(u.x, u.y, u.z, 1.0)
}
fn main() {
    mut s = vk_session()
    mut i = 0
    mut r = 0.0
    mut last = 0
    while i < 5 {
        last = vk_frame(s, r, 0.3, 0.5, 1.0)
        r = r + 0.2
        i = i + 1
    }
    mut ok = 0
    if last / 65536 > 195 { if (last / 256) % 256 > 66 { if last % 256 > 118 { ok = 1 } } }
    print(ok)
}
EOF

# Declarative `frame { bg(r,g,b) }` syntax (language level): a render frame described by
# directives, desugared at parse time to a builtin. `bg` sets the clear/background colour;
# the frame has no geometry, so the centroid is the background. frame{bg(0.9,0.3,0.6)} ->
# (229,76,153). `frame` is not a reserved word (only `frame {` triggers it).
case_ vire_declframe <<'EOF'
fn main() {
    mut px = frame {
        bg(0.9, 0.3, 0.6)
    }
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 219 { if g > 66 { if g < 86 { if b > 143 { if b < 163 { ok = 1 } } } } }
    print(ok)
}
EOF

# The generic draw surface: vk_draw(verts, ux,uy,uz,uw) — the program supplies the
# geometry AND a vec4 uniform, rendered through the compiled @fragment; uniform() reads
# the pushed value, so the centroid = the uniform color (0.5,0.6) -> (~128,~153). Proves
# geometry + uniform both come from the program, pipeline from its shader.
case_ vire_draw_generic <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut u = uniform()
    vec4(u.x, u.y, u.z, 1.0)
}
fn main() {
    mut tri = [0.0, 0.0 - 0.6, 0.6, 0.6, 0.0 - 0.6, 0.6]
    mut px = vk_draw(tri, 0.5, 0.6, 0.0, 1.0)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 120 { if r < 136 { if g > 145 { if g < 161 { ok = 1 } } } }
    print(ok)
}
EOF

# The generic draw surface WITH a reflected resource: vk_draw_tex(verts, handle, uni) —
# program geometry + a texture handle + a uniform. The @fragment's tex() reflects a
# sampler binding; the handle's texture is bound there. A 1x1 (0.2,0.9,0.3) texture
# -> centroid (~51, ~229). One generic draw covers the textured case, layout from shader.
case_ vire_draw_tex <<'EOF'
@fragment
fn fs() -> Vec4 { tex(vec2(0.5, 0.5)) }
fn main() {
    mut pixels = [0.2, 0.9, 0.3, 1.0]
    mut h = vk_texture_new(pixels, 1)
    mut tri = [0.0, 0.0 - 0.6, 0.6, 0.6, 0.0 - 0.6, 0.6]
    mut px = vk_draw_tex(tri, h, 0.0, 0.0, 0.0, 1.0)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 44 { if r < 60 { if g > 220 { if g < 238 { ok = 1 } } } }
    print(ok)
}
EOF

# The generic draw with a reflected STORAGE BUFFER: vk_draw_buf(verts, handle, uni). The
# @fragment reads buf(i) (a read-only float storage buffer at binding 0, reflected); the
# GpuBuf handle binds there via the kind switch. data=[0.3,0.7,..] -> centroid (~76,~178).
# One generic draw covers buffers too — textures AND buffers from the shader interface.
case_ vire_draw_buf <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut r = buf(0.0)
    mut g = buf(1.0)
    vec4(r, g, 0.0, 1.0)
}
fn main() {
    mut data = [0.3, 0.7, 0.9, 0.1]
    mut h = vk_buffer_new(data)
    mut tri = [0.0, 0.0 - 0.6, 0.6, 0.6, 0.0 - 0.6, 0.6]
    mut px = vk_draw_buf(tri, h, 0.0, 0.0, 0.0, 1.0)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 68 { if r < 84 { if g > 170 { if g < 186 { ok = 1 } } } }
    print(ok)
}
EOF

# The generic draw with TWO reflected sampler bindings: vk_draw_tex2(verts, h0, h1, uni).
# The @fragment reads tex() (binding 0) and tex2() (binding 1); h0/h1 bind to those two
# reflected bindings. Texture A r=0.8, texture B g=0.7 -> centroid (~204, ~179). One
# generic draw, two resources, both bound from the shader interface.
case_ vire_draw_tex2 <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut a = tex(vec2(0.5, 0.5))
    mut b = tex2(vec2(0.5, 0.5))
    vec4(a.x, b.y, 0.0, 1.0)
}
fn main() {
    mut pa = [0.8, 0.1, 0.0, 1.0]
    mut pb = [0.0, 0.7, 0.0, 1.0]
    mut ha = vk_texture_new(pa, 1)
    mut hb = vk_texture_new(pb, 1)
    mut tri = [0.0, 0.0 - 0.6, 0.6, 0.6, 0.0 - 0.6, 0.6]
    mut px = vk_draw_tex2(tri, ha, hb, 0.0, 0.0, 0.0, 1.0)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 196 { if r < 212 { if g > 170 { if g < 188 { ok = 1 } } } }
    print(ok)
}
EOF

# Unary minus in a shader body (OpFNegate). Without it a shader must write `0.0 - x`.
# frag color: r = -(-0.5) = 0.5 (~128), g = -y where y=-0.6 → 0.6 (~153).
case_ vire_unary_minus <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut y = -0.6
    vec4(-(0.0 - 0.5), -y, 0.0, 1.0)
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut ok = 0
    if r > 120 { if r < 136 { if g > 145 { if g < 161 { ok = 1 } } } }
    print(ok)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
