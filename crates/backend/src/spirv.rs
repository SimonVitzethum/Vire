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

/// A bootstrap `@mesh` stage (VK_EXT_mesh_shader): one workgroup emits the triangle
/// directly — three vertices + one primitive via `OpSetMeshOutputsEXT` — with no
/// vertex buffer and no vertex stage. This stands up the GPU-driven mesh pipeline
/// (the `VM` milestone); a Vire-authored `@mesh` body (meshlet cull + emit) replaces
/// it next. Needs SPIR-V 1.4 (assembled with `--target-env spv1.4`).
pub fn mesh_triangle_spvasm() -> String {
    MESH_TRI_SPVASM.to_string()
}

const MESH_TRI_SPVASM: &str = r###"               OpCapability MeshShadingEXT
               OpExtension "SPV_EXT_mesh_shader"
               OpMemoryModel Logical GLSL450
               OpEntryPoint MeshEXT %main "main" %gl_MeshVerticesEXT %gl_PrimitiveTriangleIndicesEXT
               OpExecutionModeId %main LocalSizeId %uint_1 %uint_1 %uint_1
               OpExecutionMode %main OutputVertices 3
               OpExecutionMode %main OutputPrimitivesEXT 1
               OpExecutionMode %main OutputTrianglesEXT
               OpDecorate %gl_MeshPerVertexEXT Block
               OpMemberDecorate %gl_MeshPerVertexEXT 0 BuiltIn Position
               OpMemberDecorate %gl_MeshPerVertexEXT 1 BuiltIn PointSize
               OpMemberDecorate %gl_MeshPerVertexEXT 2 BuiltIn ClipDistance
               OpMemberDecorate %gl_MeshPerVertexEXT 3 BuiltIn CullDistance
               OpDecorate %gl_PrimitiveTriangleIndicesEXT BuiltIn PrimitiveTriangleIndicesEXT
       %void = OpTypeVoid
          %3 = OpTypeFunction %void
       %uint = OpTypeInt 32 0
     %uint_1 = OpConstant %uint 1
     %uint_3 = OpConstant %uint 3
      %float = OpTypeFloat 32
    %v4float = OpTypeVector %float 4
%_arr_float_uint_1 = OpTypeArray %float %uint_1
%gl_MeshPerVertexEXT = OpTypeStruct %v4float %float %_arr_float_uint_1 %_arr_float_uint_1
%_arr_gl_MeshPerVertexEXT_uint_3 = OpTypeArray %gl_MeshPerVertexEXT %uint_3
%_ptr_Output__arr_gl_MeshPerVertexEXT_uint_3 = OpTypePointer Output %_arr_gl_MeshPerVertexEXT_uint_3
%gl_MeshVerticesEXT = OpVariable %_ptr_Output__arr_gl_MeshPerVertexEXT_uint_3 Output
        %int = OpTypeInt 32 1
      %int_0 = OpConstant %int 0
    %float_0 = OpConstant %float 0
%float_n0_600000024 = OpConstant %float -0.600000024
    %float_1 = OpConstant %float 1
         %21 = OpConstantComposite %v4float %float_0 %float_n0_600000024 %float_0 %float_1
%_ptr_Output_v4float = OpTypePointer Output %v4float
      %int_1 = OpConstant %int 1
%float_0_600000024 = OpConstant %float 0.600000024
         %26 = OpConstantComposite %v4float %float_0_600000024 %float_0_600000024 %float_0 %float_1
      %int_2 = OpConstant %int 2
         %29 = OpConstantComposite %v4float %float_n0_600000024 %float_0_600000024 %float_0 %float_1
     %v3uint = OpTypeVector %uint 3
%_arr_v3uint_uint_1 = OpTypeArray %v3uint %uint_1
%_ptr_Output__arr_v3uint_uint_1 = OpTypePointer Output %_arr_v3uint_uint_1
%gl_PrimitiveTriangleIndicesEXT = OpVariable %_ptr_Output__arr_v3uint_uint_1 Output
     %uint_0 = OpConstant %uint 0
     %uint_2 = OpConstant %uint 2
         %37 = OpConstantComposite %v3uint %uint_0 %uint_1 %uint_2
%_ptr_Output_v3uint = OpTypePointer Output %v3uint
       %main = OpFunction %void None %3
          %5 = OpLabel
               OpSetMeshOutputsEXT %uint_3 %uint_1
         %23 = OpAccessChain %_ptr_Output_v4float %gl_MeshVerticesEXT %int_0 %int_0
               OpStore %23 %21
         %27 = OpAccessChain %_ptr_Output_v4float %gl_MeshVerticesEXT %int_1 %int_0
               OpStore %27 %26
         %30 = OpAccessChain %_ptr_Output_v4float %gl_MeshVerticesEXT %int_2 %int_0
               OpStore %30 %29
         %39 = OpAccessChain %_ptr_Output_v3uint %gl_PrimitiveTriangleIndicesEXT %int_0
               OpStore %39 %37
               OpReturn
               OpFunctionEnd
"###;

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
