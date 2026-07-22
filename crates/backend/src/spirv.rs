//! SPIR-V shader generation for `@vulkan` (VS milestone — "Vire is the shader
//! language"). The graphics Shader flavor of SPIR-V is NOT emitted by
//! `llc -march=spirv64` (that is the Kernel/compute flavor), so these produce
//! SPIR-V **assembly** text that the driver assembles with `spirv-as` (validated
//! by `spirv-val`). First step: a fixed triangle vertex stage + a fragment stage
//! whose constant color comes from a Vire `@fragment fn` body (`vec4(r,g,b,a)`).
//! Next: emit both stages from the shader IR (structured control flow, varyings).

/// The triangle vertex stage — three clip-space positions selected by
/// `gl_VertexIndex`, `gl_Position` written. Fixed for now (bootstrap); a
/// Vire-authored `@vertex fn` will replace it.
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

const VERT_SPVASM: &str = r###"
; SPIR-V
; Version: 1.0
; Generator: Google Shaderc over Glslang; 11
; Bound: 40
; Schema: 0
               OpCapability Shader
          %1 = OpExtInstImport "GLSL.std.450"
               OpMemoryModel Logical GLSL450
               OpEntryPoint Vertex %main "main" %_ %gl_VertexIndex
               OpSource GLSL 450
               OpSourceExtension "GL_GOOGLE_cpp_style_line_directive"
               OpSourceExtension "GL_GOOGLE_include_directive"
               OpName %main "main"
               OpName %P "P"
               OpName %gl_PerVertex "gl_PerVertex"
               OpMemberName %gl_PerVertex 0 "gl_Position"
               OpMemberName %gl_PerVertex 1 "gl_PointSize"
               OpMemberName %gl_PerVertex 2 "gl_ClipDistance"
               OpMemberName %gl_PerVertex 3 "gl_CullDistance"
               OpName %_ ""
               OpName %gl_VertexIndex "gl_VertexIndex"
               OpDecorate %gl_PerVertex Block
               OpMemberDecorate %gl_PerVertex 0 BuiltIn Position
               OpMemberDecorate %gl_PerVertex 1 BuiltIn PointSize
               OpMemberDecorate %gl_PerVertex 2 BuiltIn ClipDistance
               OpMemberDecorate %gl_PerVertex 3 BuiltIn CullDistance
               OpDecorate %gl_VertexIndex BuiltIn VertexIndex
       %void = OpTypeVoid
          %3 = OpTypeFunction %void
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
       %uint = OpTypeInt 32 0
     %uint_3 = OpConstant %uint 3
%_arr_v2float_uint_3 = OpTypeArray %v2float %uint_3
%_ptr_Private__arr_v2float_uint_3 = OpTypePointer Private %_arr_v2float_uint_3
          %P = OpVariable %_ptr_Private__arr_v2float_uint_3 Private
    %float_0 = OpConstant %float 0
%float_n0_600000024 = OpConstant %float -0.600000024
         %15 = OpConstantComposite %v2float %float_0 %float_n0_600000024
%float_0_600000024 = OpConstant %float 0.600000024
         %17 = OpConstantComposite %v2float %float_0_600000024 %float_0_600000024
         %18 = OpConstantComposite %v2float %float_n0_600000024 %float_0_600000024
         %19 = OpConstantComposite %_arr_v2float_uint_3 %15 %17 %18
    %v4float = OpTypeVector %float 4
     %uint_1 = OpConstant %uint 1
%_arr_float_uint_1 = OpTypeArray %float %uint_1
%gl_PerVertex = OpTypeStruct %v4float %float %_arr_float_uint_1 %_arr_float_uint_1
%_ptr_Output_gl_PerVertex = OpTypePointer Output %gl_PerVertex
          %_ = OpVariable %_ptr_Output_gl_PerVertex Output
        %int = OpTypeInt 32 1
      %int_0 = OpConstant %int 0
%_ptr_Input_int = OpTypePointer Input %int
%gl_VertexIndex = OpVariable %_ptr_Input_int Input
%_ptr_Private_v2float = OpTypePointer Private %v2float
    %float_1 = OpConstant %float 1
%_ptr_Output_v4float = OpTypePointer Output %v4float
       %main = OpFunction %void None %3
          %5 = OpLabel
               OpStore %P %19
         %30 = OpLoad %int %gl_VertexIndex
         %32 = OpAccessChain %_ptr_Private_v2float %P %30
         %33 = OpLoad %v2float %32
         %35 = OpCompositeExtract %float %33 0
         %36 = OpCompositeExtract %float %33 1
         %37 = OpCompositeConstruct %v4float %35 %36 %float_0 %float_1
         %39 = OpAccessChain %_ptr_Output_v4float %_ %int_0
               OpStore %39 %37
               OpReturn
               OpFunctionEnd
"###;
