//! SPIR-V shader generation for `@vulkan` (VS milestone — "Vire is the shader
//! language"). The graphics Shader flavor of SPIR-V is NOT emitted by
//! `llc -march=spirv64` (that is the Kernel/compute flavor), so these produce
//! SPIR-V **assembly** text that the driver assembles with `spirv-as` (validated
//! by `spirv-val`). First step: a fixed triangle vertex stage + a fragment stage
//! whose constant color comes from a Vire `@fragment fn` body (`vec4(r,g,b,a)`).
//! Next: emit both stages from the shader IR (structured control flow, varyings).

/// The default vertex stage — reads a 2D position from the vertex buffer
/// (attribute `Location 0`) and writes `gl_Position`. Used when a program defines
/// no `@vertex fn`; the geometry then comes from the vertex buffer the runtime
/// uploads (`jrt_vk_triangle`'s default corners, or `vk_mesh`'s Vire data). A
/// Vire-authored `@vertex fn` (crates/vire/src/shader.rs) reads the same attribute.
pub fn triangle_vertex_spvasm() -> String {
    VERT_SPVASM.to_string()
}

/// A fragment stage that outputs a single constant color at `Location 0`.
/// `rgba` comes from the Vire `@fragment fn`'s `vec4(...)` literal (or the default).
pub fn constant_fragment_spvasm(rgba: [f32; 4]) -> String {
    // `spirv-as` parses full-precision decimals; format enough digits to round-trip.
    let f = |x: f32| format!("{:.9}", x);
    format!(
"               OpCapability Shader
               OpMemoryModel Logical GLSL450
               OpEntryPoint Fragment %main \"main\" %color
               OpExecutionMode %main OriginUpperLeft
               OpDecorate %color Location 0
       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
      %float = OpTypeFloat 32
    %v4float = OpTypeVector %float 4
       %optr = OpTypePointer Output %v4float
      %color = OpVariable %optr Output
         %cr = OpConstant %float {}
         %cg = OpConstant %float {}
         %cb = OpConstant %float {}
         %ca = OpConstant %float {}
        %col = OpConstantComposite %v4float %cr %cg %cb %ca
       %main = OpFunction %void None %fnty
        %lbl = OpLabel
               OpStore %color %col
               OpReturn
               OpFunctionEnd
",
        f(rgba[0]), f(rgba[1]), f(rgba[2]), f(rgba[3])
    )
}

const VERT_SPVASM: &str = r###"               OpCapability Shader
               OpMemoryModel Logical GLSL450
               OpEntryPoint Vertex %main "main" %out %pos_in
               OpDecorate %glpv Block
               OpMemberDecorate %glpv 0 BuiltIn Position
               OpDecorate %pos_in Location 0
       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v4float = OpTypeVector %float 4
       %glpv = OpTypeStruct %v4float
     %outptr = OpTypePointer Output %glpv
        %out = OpVariable %outptr Output
      %inptr = OpTypePointer Input %v2float
     %pos_in = OpVariable %inptr Input
        %int = OpTypeInt 32 1
      %int_0 = OpConstant %int 0
         %f0 = OpConstant %float 0
         %f1 = OpConstant %float 1
     %ov4ptr = OpTypePointer Output %v4float
       %main = OpFunction %void None %fnty
        %lbl = OpLabel
        %pos = OpLoad %v2float %pos_in
         %px = OpCompositeExtract %float %pos 0
         %py = OpCompositeExtract %float %pos 1
        %pp4 = OpCompositeConstruct %v4float %px %py %f0 %f1
         %gp = OpAccessChain %ov4ptr %out %int_0
               OpStore %gp %pp4
               OpReturn
               OpFunctionEnd
"###;
