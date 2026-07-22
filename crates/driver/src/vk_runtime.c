/* Vire @vulkan runtime (V2). Two entry points share one pipeline builder:
 *   jrt_vk_triangle()      — headless render + pixel self-verify (CI, no display).
 *   jrt_vk_window(frames)  — open a window, present the triangle (frames=0: until
 *                            closed). Needs a display + GLFW.
 *   jrt_vk_mesh(verts,n)   — headless render of Vire-supplied geometry (a vertex
 *                            buffer), same centroid readback.
 * The graphics pipeline reads positions from a vertex buffer (attribute Location 0);
 * `jrt_vk_triangle`/`jrt_vk_window` upload the built-in corners, `jrt_vk_mesh`
 * uploads Vire data, `jrt_vk_mesh_c` adds a per-vertex color (Location 1, read via
 * attr_color()). Shader SPIR-V is Vire-authored (crates/vire/src/shader.rs +
 * crates/backend/src/spirv.rs). See language/GPU-VULKAN.md. Vendor-neutral.
 */
#define GLFW_INCLUDE_VULKAN
#include <GLFW/glfw3.h>
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <time.h>

#define CK(x) do { if((x)!=VK_SUCCESS) return 0; } while(0)

/* Shader SPIR-V is generated at Vire build time (crates/backend/src/spirv.rs ->
 * spirv-as) into vk_shaders.c and linked alongside — the @fragment color comes
 * from the Vire source. Declared extern here (word counts as *_N). */
extern const uint32_t VK_TRI_VERT[]; extern const unsigned VK_TRI_VERT_N;
extern const uint32_t VK_TRI_FRAG[]; extern const unsigned VK_TRI_FRAG_N;
extern const uint32_t VK_MESH_TRI[]; extern const unsigned VK_MESH_TRI_N;
extern const uint32_t VK_TASK_TRI[]; extern const unsigned VK_TASK_TRI_N; /* N=0 → no task stage */
extern const uint32_t VK_BUILD_COMP[]; extern const unsigned VK_BUILD_COMP_N; /* N=0 → no compute builder */
extern const uint32_t VK_GPUVK_COMP[]; extern const unsigned VK_GPUVK_COMP_N; /* N=0 → no @gpuvk map */

/* forward decls (defined below) */
static uint32_t find_mem(VkPhysicalDevice pd, uint32_t bits, VkMemoryPropertyFlags want);
static VkShaderModule shmod(VkDevice d, const uint32_t *code, size_t n);

/* gpuvk_run(data, n): vendor-neutral Vulkan compute. Runs the program's @gpuvk map
 * over `n` f64 elements in place on ANY Vulkan device (no mesh-shader needed): upload
 * as f32 to an SSBO, dispatch the compute shader (bounds-guarded), read back. Returns
 * 0, or -2 if no Vulkan device / -1 on failure. */
int64_t jrt_vk_gpuvk_run(double *data, int64_t n) {
    if(!data || n <= 0) return -1;
    if(VK_GPUVK_COMP_N == 0) return -1;
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_2};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0); if(!nd){ vkDestroyInstance(inst,0); return -2; }
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(inst,&nd,pds);
    VkPhysicalDevice pd=0; uint32_t qf=0;
    for(uint32_t d=0; d<nd && !pd; d++){
        uint32_t m=0; vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&m,0);
        VkQueueFamilyProperties *qs=malloc(m*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&m,qs);
        for(uint32_t i=0;i<m;i++) if(qs[i].queueFlags&VK_QUEUE_COMPUTE_BIT){ pd=pds[d]; qf=i; break; } free(qs);
    }
    free(pds); if(!pd){ vkDestroyInstance(inst,0); return -2; }
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qf,.queueCount=1,.pQueuePriorities=&pr};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci};
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);

    VkDeviceSize sz=(VkDeviceSize)n*sizeof(float);
    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=sz,.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer buf; CK(vkCreateBuffer(dev,&bi,0,&buf));
    VkMemoryRequirements br; vkGetBufferMemoryRequirements(dev,buf,&br);
    uint32_t bt=find_mem(pd,br.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(bt==~0u) return -1;
    VkMemoryAllocateInfo bm={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=br.size,.memoryTypeIndex=bt};
    VkDeviceMemory bmem; CK(vkAllocateMemory(dev,&bm,0,&bmem)); vkBindBufferMemory(dev,buf,bmem,0);
    float *host; CK(vkMapMemory(dev,bmem,0,sz,0,(void**)&host));
    for(int64_t i=0;i<n;i++) host[i]=(float)data[i];
    vkUnmapMemory(dev,bmem);

    VkDescriptorSetLayoutBinding dslb={.binding=0,.descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.descriptorCount=1,.stageFlags=VK_SHADER_STAGE_COMPUTE_BIT};
    VkDescriptorSetLayoutCreateInfo dslci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,.bindingCount=1,.pBindings=&dslb};
    VkDescriptorSetLayout dsl; CK(vkCreateDescriptorSetLayout(dev,&dslci,0,&dsl));
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.descriptorCount=1};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    VkDescriptorBufferInfo dbi={.buffer=buf,.offset=0,.range=sz};
    VkWriteDescriptorSet wds={.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.pBufferInfo=&dbi};
    vkUpdateDescriptorSets(dev,1,&wds,0,0);

    VkPushConstantRange pcr={.stageFlags=VK_SHADER_STAGE_COMPUTE_BIT,.offset=0,.size=4};
    VkPipelineLayoutCreateInfo plci={.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,.setLayoutCount=1,.pSetLayouts=&dsl,.pushConstantRangeCount=1,.pPushConstantRanges=&pcr};
    VkPipelineLayout pl; CK(vkCreatePipelineLayout(dev,&plci,0,&pl));
    VkShaderModule cm=shmod(dev,VK_GPUVK_COMP,VK_GPUVK_COMP_N*4); if(!cm) return -1;
    VkComputePipelineCreateInfo cpci={.sType=VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage={.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_COMPUTE_BIT,.module=cm,.pName="main"},.layout=pl};
    VkPipeline pipe=0; vkCreateComputePipelines(dev,0,1,&cpci,0,&pipe); vkDestroyShaderModule(dev,cm,0); if(!pipe) return -1;

    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; CK(vkAllocateCommandBuffers(dev,&cai,&cmd));
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&cbi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&dset,0,0);
    uint32_t cnt=(uint32_t)n;
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,4,&cnt);
    vkCmdDispatch(cmd,(cnt+63)/64,1,1);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    CK(vkMapMemory(dev,bmem,0,sz,0,(void**)&host));
    for(int64_t i=0;i<n;i++) data[i]=(double)host[i];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return 0;
}

/* Does a physical device advertise a given extension? */
static int has_ext(VkPhysicalDevice pd, const char *name) {
    uint32_t n=0; vkEnumerateDeviceExtensionProperties(pd,0,&n,0);
    VkExtensionProperties *e=malloc(n*sizeof*e); vkEnumerateDeviceExtensionProperties(pd,0,&n,e);
    int found=0; for(uint32_t i=0;i<n;i++) if(!strcmp(e[i].extensionName,name)){found=1;break;}
    free(e); return found;
}

static uint32_t find_mem(VkPhysicalDevice pd, uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
    for(uint32_t i=0;i<mp.memoryTypeCount;i++)
        if((bits&(1u<<i)) && (mp.memoryTypes[i].propertyFlags&want)==want) return i;
    return ~0u;
}
static VkShaderModule shmod(VkDevice d, const uint32_t *code, size_t n) {
    VkShaderModuleCreateInfo ci={.sType=VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,.codeSize=n,.pCode=code};
    VkShaderModule m; return vkCreateShaderModule(d,&ci,0,&m)==VK_SUCCESS?m:0;
}

/* The default triangle, in clip space — supplied to the vertex stage as a vertex
 * buffer (the vertex shader reads attribute Location 0). `vk_mesh` replaces this
 * with Vire-supplied geometry. */
static const float DEFAULT_TRI[6] = { 0.0f,-0.6f,  0.6f,0.6f,  -0.6f,0.6f };

/* Upload `nfloats` f32 values (interleaved vertex attributes) into a host-visible
 * vertex buffer. Returns 1 on success (out params set), 0 on failure. */
static int make_vbuf(VkDevice dev, VkPhysicalDevice pd, const float *data, uint32_t nfloats,
                     VkBuffer *out_buf, VkDeviceMemory *out_mem) {
    VkDeviceSize sz = (VkDeviceSize)nfloats * sizeof(float); if(sz==0) sz=8;
    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=sz,.usage=VK_BUFFER_USAGE_VERTEX_BUFFER_BIT};
    if(vkCreateBuffer(dev,&bi,0,out_buf)!=VK_SUCCESS) return 0;
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,*out_buf,&mr);
    uint32_t t=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    if(t==~0u) return 0;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=t};
    if(vkAllocateMemory(dev,&ma,0,out_mem)!=VK_SUCCESS) return 0;
    vkBindBufferMemory(dev,*out_buf,*out_mem,0);
    void *p; if(vkMapMemory(dev,*out_mem,0,sz,0,&p)!=VK_SUCCESS) return 0;
    memcpy(p, data, (size_t)nfloats*sizeof(float)); vkUnmapMemory(dev,*out_mem);
    return 1;
}

/* The one shared piece: build the triangle graphics pipeline for a render pass +
 * extent. Layout is empty (no descriptors); shaders are the embedded SPIR-V. */
static VkPipeline build_pipeline(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, int colored) {
    VkShaderModule vs=shmod(dev,VK_TRI_VERT,VK_TRI_VERT_N*4), fs=shmod(dev,VK_TRI_FRAG,VK_TRI_FRAG_N*4);
    if(!vs||!fs) return 0;
    VkPipelineShaderStageCreateInfo st[2]={
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_VERTEX_BIT,.module=vs,.pName="main"},
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_FRAGMENT_BIT,.module=fs,.pName="main"}};
    /* Vertex buffer at binding 0. Position-only: (x,y) f32 at Location 0, stride 8.
     * Colored (vk_mesh_c): + a per-vertex color (r,g,b) at Location 1, offset 8,
     * stride 20 — read in the vertex shader via attr_color(). */
    VkVertexInputBindingDescription vbind={.binding=0,.stride=(colored?5:2)*sizeof(float),.inputRate=VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription vattr[2]={
        {.location=0,.binding=0,.format=VK_FORMAT_R32G32_SFLOAT,.offset=0},
        {.location=1,.binding=0,.format=VK_FORMAT_R32G32B32_SFLOAT,.offset=2*sizeof(float)}};
    VkPipelineVertexInputStateCreateInfo vi={.sType=VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount=1,.pVertexBindingDescriptions=&vbind,
        .vertexAttributeDescriptionCount=(uint32_t)(colored?2:1),.pVertexAttributeDescriptions=vattr};
    VkPipelineInputAssemblyStateCreateInfo ia={.sType=VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport vp={0,0,(float)w,(float)h,0,1}; VkRect2D sc={{0,0},{w,h}};
    VkPipelineViewportStateCreateInfo vps={.sType=VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,.viewportCount=1,.pViewports=&vp,.scissorCount=1,.pScissors=&sc};
    VkPipelineRasterizationStateCreateInfo rs={.sType=VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,.polygonMode=VK_POLYGON_MODE_FILL,.cullMode=VK_CULL_MODE_NONE,.frontFace=VK_FRONT_FACE_COUNTER_CLOCKWISE,.lineWidth=1.0f};
    VkPipelineMultisampleStateCreateInfo ms={.sType=VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState cba={.colorWriteMask=0xf};
    VkPipelineColorBlendStateCreateInfo cb={.sType=VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,.attachmentCount=1,.pAttachments=&cba};
    VkPipelineLayoutCreateInfo plci={.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    if(vkCreatePipelineLayout(dev,&plci,0,out_layout)!=VK_SUCCESS) return 0;
    VkGraphicsPipelineCreateInfo gp={.sType=VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,.stageCount=2,.pStages=st,
        .pVertexInputState=&vi,.pInputAssemblyState=&ia,.pViewportState=&vps,.pRasterizationState=&rs,
        .pMultisampleState=&ms,.pColorBlendState=&cb,.layout=*out_layout,.renderPass=rp,.subpass=0};
    VkPipeline pipe=0; vkCreateGraphicsPipelines(dev,0,1,&gp,0,&pipe);
    vkDestroyShaderModule(dev,vs,0); vkDestroyShaderModule(dev,fs,0);
    return pipe;
}
/* The GPU-driven pipeline: a mesh stage (no vertex input, no input assembly — the
 * mesh shader emits vertices + primitives itself) + the fragment stage. */
static VkPipeline build_mesh_pipeline(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, VkDescriptorSetLayout dsl) {
    VkShaderModule ms=shmod(dev,VK_MESH_TRI,VK_MESH_TRI_N*4), fs=shmod(dev,VK_TRI_FRAG,VK_TRI_FRAG_N*4);
    if(!ms||!fs) return 0;
    /* Optional amplification (task) stage — prepended when the program has an @task. */
    VkShaderModule ts=0; VkPipelineShaderStageCreateInfo st[3]; uint32_t nst=0;
    if(VK_TASK_TRI_N>0){
        ts=shmod(dev,VK_TASK_TRI,VK_TASK_TRI_N*4); if(!ts) return 0;
        st[nst++]=(VkPipelineShaderStageCreateInfo){.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_TASK_BIT_EXT,.module=ts,.pName="main"};
    }
    st[nst++]=(VkPipelineShaderStageCreateInfo){.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_MESH_BIT_EXT,.module=ms,.pName="main"};
    st[nst++]=(VkPipelineShaderStageCreateInfo){.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_FRAGMENT_BIT,.module=fs,.pName="main"};
    VkViewport vp={0,0,(float)w,(float)h,0,1}; VkRect2D sc={{0,0},{w,h}};
    VkPipelineViewportStateCreateInfo vps={.sType=VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,.viewportCount=1,.pViewports=&vp,.scissorCount=1,.pScissors=&sc};
    VkPipelineRasterizationStateCreateInfo rs={.sType=VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,.polygonMode=VK_POLYGON_MODE_FILL,.cullMode=VK_CULL_MODE_NONE,.frontFace=VK_FRONT_FACE_COUNTER_CLOCKWISE,.lineWidth=1.0f};
    VkPipelineMultisampleStateCreateInfo msi={.sType=VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState cba={.colorWriteMask=0xf};
    VkPipelineColorBlendStateCreateInfo cb={.sType=VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,.attachmentCount=1,.pAttachments=&cba};
    /* A 16-byte push constant (the frustum plane) for the amplification/task cull.
     * The range's stages must be a subset of the pipeline's — include TASK only when
     * a task stage exists, else the mesh stage. */
    VkShaderStageFlags pcStages = (VK_TASK_TRI_N>0) ? (VK_SHADER_STAGE_TASK_BIT_EXT|VK_SHADER_STAGE_MESH_BIT_EXT) : VK_SHADER_STAGE_MESH_BIT_EXT;
    VkPushConstantRange pcr={.stageFlags=pcStages,.offset=0,.size=16};
    VkPipelineLayoutCreateInfo plci={.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,.pushConstantRangeCount=1,.pPushConstantRanges=&pcr,
        .setLayoutCount=(dsl?1u:0u),.pSetLayouts=(dsl?&dsl:0)};
    if(vkCreatePipelineLayout(dev,&plci,0,out_layout)!=VK_SUCCESS) return 0;
    VkGraphicsPipelineCreateInfo gp={.sType=VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,.stageCount=nst,.pStages=st,
        .pViewportState=&vps,.pRasterizationState=&rs,.pMultisampleState=&msi,.pColorBlendState=&cb,
        .layout=*out_layout,.renderPass=rp,.subpass=0};
    VkPipeline pipe=0; vkCreateGraphicsPipelines(dev,0,1,&gp,0,&pipe);
    vkDestroyShaderModule(dev,ms,0); vkDestroyShaderModule(dev,fs,0); if(ts) vkDestroyShaderModule(dev,ts,0);
    return pipe;
}
static VkRenderPass build_rp(VkDevice dev, VkFormat fmt, VkImageLayout final) {
    VkAttachmentDescription att={.format=fmt,.samples=VK_SAMPLE_COUNT_1_BIT,
        .loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR,.storeOp=VK_ATTACHMENT_STORE_OP_STORE,
        .stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE,.stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE,
        .initialLayout=VK_IMAGE_LAYOUT_UNDEFINED,.finalLayout=final};
    VkAttachmentReference ref={0,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub={.pipelineBindPoint=VK_PIPELINE_BIND_POINT_GRAPHICS,.colorAttachmentCount=1,.pColorAttachments=&ref};
    VkSubpassDependency dep={.srcSubpass=VK_SUBPASS_EXTERNAL,.dstSubpass=0,
        .srcStageMask=VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,.dstStageMask=VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
        .dstAccessMask=VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT};
    VkRenderPassCreateInfo ci={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,.attachmentCount=1,.pAttachments=&att,.subpassCount=1,.pSubpasses=&sub,.dependencyCount=1,.pDependencies=&dep};
    VkRenderPass rp=0; vkCreateRenderPass(dev,&ci,0,&rp); return rp;
}
static void rec_draw(VkCommandBuffer cmd, VkRenderPass rp, VkFramebuffer fb, VkPipeline pipe,
                     uint32_t w, uint32_t h, VkBuffer vbuf, uint32_t nverts) {
    VkCommandBufferBeginInfo bi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&bi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{w,h}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off);
    vkCmdDraw(cmd,nverts,1,0,0);
    vkCmdEndRenderPass(cmd);
}

/* ---- headless: render `nverts` triangle-list vertices (interleaved f32 x,y) to an
 *      image, read back; returns the centroid pixel packed as 0xRRGGBB (so callers
 *      can check the @fragment color), or -1 on failure ---- */
static int64_t render_headless(const float *data, uint32_t nverts, uint32_t fpv) {
    enum { W=256, H=256 };
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0); if(!nd) return 0;
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(inst,&nd,pds);
    VkPhysicalDevice pd=pds[0]; free(pds); uint32_t qf=0;
    { uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(pd,&n,0);
      VkQueueFamilyProperties *qs=malloc(n*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pd,&n,qs);
      int f=0; for(uint32_t i=0;i<n;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){qf=i;f=1;break;} free(qs); if(!f) return 0; }
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qf,.queueCount=1,.pQueuePriorities=&pr};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci};
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);

    VkImageCreateInfo ii={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage img; CK(vkCreateImage(dev,&ii,0,&img));
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,img,&mr);
    uint32_t it=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return 0;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    VkDeviceMemory im; CK(vkAllocateMemory(dev,&ma,0,&im)); vkBindImageMemory(dev,img,im,0);
    VkImageViewCreateInfo vi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView view; CK(vkCreateImageView(dev,&vi,0,&view));
    VkRenderPass rp=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL); if(!rp) return 0;
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&view,.width=W,.height=H,.layers=1};
    VkFramebuffer fb; CK(vkCreateFramebuffer(dev,&fbi,0,&fb));
    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl,fpv==5); if(!pipe) return 0;
    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,data,nverts*fpv,&vbuf,&vmem)) return 0;

    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=W*H*4,.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf; CK(vkCreateBuffer(dev,&bi,0,&buf));
    VkMemoryRequirements br; vkGetBufferMemoryRequirements(dev,buf,&br);
    uint32_t bt=find_mem(pd,br.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(bt==~0u) return 0;
    VkMemoryAllocateInfo bm={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=br.size,.memoryTypeIndex=bt};
    VkDeviceMemory bmem; CK(vkAllocateMemory(dev,&bm,0,&bmem)); vkBindBufferMemory(dev,buf,bmem,0);

    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; CK(vkAllocateCommandBuffers(dev,&cai,&cmd));
    rec_draw(cmd,rp,fb,pipe,W,H,vbuf,nverts);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int cx=W/2, cy=(int)(H*0.55); unsigned char *c=&px[(cy*W+cx)*4], *tl=&px[(5*W+5)*4];
    /* centroid = the triangle (fragment color); corner must be the clear color. */
    int64_t packed = ((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    int corner_clear = tl[0]<60 && tl[1]<60 && tl[2]<60;
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0); vkDestroyFramebuffer(dev,fb,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return corner_clear ? packed : -1;
}

/* The default triangle, from the compile-time corner buffer. */
int64_t jrt_vk_triangle(void) { return render_headless(DEFAULT_TRI, 3, 2); }

/* ---- benchmark: init once, render `frames` headless mesh-shader frames, return the
 *      total nanoseconds spent in the submit+wait loop (steady-state per-frame CPU
 *      cost, no re-init). Matches the C++/Rust baselines in benchmarks/vulkan/. Uses
 *      the mesh pipeline (VK_EXT_mesh_shader); returns -2 without support. ---- */
static int64_t now_ns(void){ struct timespec t; clock_gettime(CLOCK_MONOTONIC,&t); return (int64_t)t.tv_sec*1000000000LL + t.tv_nsec; }
int64_t jrt_vk_bench(int64_t frames) {
    if(frames <= 0) return -1;
    enum { W=256, H=256 };
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_3};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0); if(!nd) return -2;
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(inst,&nd,pds);
    VkPhysicalDevice pd=0; uint32_t qf=0;
    for(uint32_t d=0; d<nd && !pd; d++){ if(!has_ext(pds[d],VK_EXT_MESH_SHADER_EXTENSION_NAME)) continue;
        uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,0);
        VkQueueFamilyProperties *qs=malloc(n*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,qs);
        for(uint32_t i=0;i<n;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){pd=pds[d];qf=i;break;} free(qs); }
    free(pds); if(!pd){ vkDestroyInstance(inst,0); return -2; }
    VkPhysicalDeviceMeshShaderFeaturesEXT mf={.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MESH_SHADER_FEATURES_EXT,.meshShader=VK_TRUE};
    const char *dext[]={VK_EXT_MESH_SHADER_EXTENSION_NAME};
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qf,.queueCount=1,.pQueuePriorities=&pr};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.pNext=&mf,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci,.enabledExtensionCount=1,.ppEnabledExtensionNames=dext};
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);
    PFN_vkCmdDrawMeshTasksEXT draw_mesh=(PFN_vkCmdDrawMeshTasksEXT)vkGetDeviceProcAddr(dev,"vkCmdDrawMeshTasksEXT");
    if(!draw_mesh){ vkDestroyDevice(dev,0); vkDestroyInstance(inst,0); return -2; }
    VkImageCreateInfo ii={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage img; CK(vkCreateImage(dev,&ii,0,&img));
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,img,&mr);
    uint32_t it=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return -1;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    VkDeviceMemory im; CK(vkAllocateMemory(dev,&ma,0,&im)); vkBindImageMemory(dev,img,im,0);
    VkImageViewCreateInfo ivi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView view; CK(vkCreateImageView(dev,&ivi,0,&view));
    VkRenderPass rp=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL); if(!rp) return -1;
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&view,.width=W,.height=H,.layers=1};
    VkFramebuffer fb; CK(vkCreateFramebuffer(dev,&fbi,0,&fb));
    VkPipelineLayout pl; VkPipeline pipe=build_mesh_pipeline(dev,rp,W,H,&pl,0); if(!pipe) return -1;
    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; CK(vkAllocateCommandBuffers(dev,&cai,&cmd));
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    draw_mesh(cmd,1,1,1);
    vkCmdEndRenderPass(cmd);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    /* one warm-up frame, then time `frames` submit+wait cycles */
    vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull); vkResetFences(dev,1,&fence);
    int64_t t0=now_ns();
    for(int64_t f=0;f<frames;f++){ vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull); vkResetFences(dev,1,&fence); }
    int64_t elapsed=now_ns()-t0;
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0); vkDestroyFramebuffer(dev,fb,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return elapsed;
}

/* Shared body for the mesh builtins: convert `nfloats` f64 values to f32, render a
 * triangle list of `nverts` vertices (`fpv` floats each), return the centroid pixel. */
static int64_t mesh_render(const double *verts, int64_t nfloats, uint32_t fpv) {
    if(!verts || nfloats < (int64_t)(3*fpv) || (nfloats % fpv)!=0) return -1;   /* need >=3 vertices */
    uint32_t nverts=(uint32_t)(nfloats/fpv);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    int64_t r=render_headless(f, nverts, fpv);
    free(f);
    return r;
}

/* vk_mesh(verts): render Vire-supplied geometry — `verts` is a flat [Float] array of
 * interleaved (x,y) clip-space positions (2 per vertex), f64 in Vire. Proves the
 * geometry comes from Vire data, not the built-in corners. */
int64_t jrt_vk_mesh(const double *verts, int64_t nfloats) { return mesh_render(verts, nfloats, 2); }

/* vk_mesh_c(verts): per-vertex attributes — `verts` interleaves (x,y, r,g,b), 5 per
 * vertex. The vertex shader reads the color at Location 1 via attr_color(); typically
 * `out_color(attr_color())` forwards it, so each vertex carries its own color and the
 * rasterizer interpolates (the classic RGB-corner triangle). Typed stage I/O. */
int64_t jrt_vk_mesh_c(const double *verts, int64_t nfloats) { return mesh_render(verts, nfloats, 5); }

/* vk_mesh_scene(offsets): MANY meshlets from a Vire scene buffer. The N (x,y) offsets
 * are uploaded to an SSBO (binding 0); one indirect dispatch of N mesh workgroups
 * (vkCmdDrawMeshTasksIndirectEXT) draws N meshlets — each mesh workgroup reads its own
 * offset via meshlet_offset() (scene[gl_WorkGroupID.x]). Returns a 2-bit mask: bit 0 if
 * the left quarter is drawn, bit 1 if the right quarter is — so a caller can verify
 * multiple meshlets landed. -2 if no mesh-shader device, -1 on failure. */
/* Render a scene of meshlets. If `builder` is set, a @compute shader fills the scene
 * SSBO on the GPU first (bcount meshlets, `offs` ignored); otherwise the host offsets
 * are uploaded. `plane` is the frustum plane for the @task cull (permissive if none). */
static int64_t scene_render(const double *offs, int64_t nfloats, const float plane[4], int builder, uint32_t bcount) {
    enum { W=256, H=256 };
    if(!builder && (!offs || nfloats < 2 || (nfloats & 1))) return -1;
    uint32_t nmesh = builder ? bcount : (uint32_t)(nfloats/2);
    if(nmesh==0) return -1;
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_3};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0); if(!nd) return -2;
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(inst,&nd,pds);
    VkPhysicalDevice pd=0; uint32_t qf=0;
    for(uint32_t d=0; d<nd && !pd; d++) {
        if(!has_ext(pds[d], VK_EXT_MESH_SHADER_EXTENSION_NAME)) continue;
        uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,0);
        VkQueueFamilyProperties *qs=malloc(n*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,qs);
        for(uint32_t i=0;i<n;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){ pd=pds[d]; qf=i; break; }
        free(qs);
    }
    free(pds); if(!pd){ vkDestroyInstance(inst,0); return -2; }

    VkPhysicalDeviceMeshShaderFeaturesEXT mf={.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MESH_SHADER_FEATURES_EXT,.meshShader=VK_TRUE};
    const char *dext[]={VK_EXT_MESH_SHADER_EXTENSION_NAME};
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qf,.queueCount=1,.pQueuePriorities=&pr};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.pNext=&mf,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci,.enabledExtensionCount=1,.ppEnabledExtensionNames=dext};
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);
    PFN_vkCmdDrawMeshTasksIndirectEXT draw_indirect=(PFN_vkCmdDrawMeshTasksIndirectEXT)vkGetDeviceProcAddr(dev,"vkCmdDrawMeshTasksIndirectEXT");
    if(!draw_indirect){ vkDestroyDevice(dev,0); vkDestroyInstance(inst,0); return -2; }

    /* Scene SSBO: N typed Meshlet records (std430: vec2 offset @0, vec2 cone @8 —
     * 16 bytes each), host-visible. The compute builder fills it on the GPU (left
     * uninitialized here); otherwise upload the host offsets (cone left zero). */
    VkDeviceSize ssz=(VkDeviceSize)nmesh*4*sizeof(float); if(ssz==0) ssz=16;
    VkBufferCreateInfo sbi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=ssz,.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer ssbo; CK(vkCreateBuffer(dev,&sbi,0,&ssbo));
    VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev,ssbo,&smr);
    uint32_t smt=find_mem(pd,smr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(smt==~0u) return -1;
    VkMemoryAllocateInfo sma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=smr.size,.memoryTypeIndex=smt};
    VkDeviceMemory smem; CK(vkAllocateMemory(dev,&sma,0,&smem)); vkBindBufferMemory(dev,ssbo,smem,0);
    if(!builder){
        float *rec=calloc((size_t)nmesh*4,sizeof(float)); if(!rec) return -1;
        for(uint32_t i=0;i<nmesh;i++){ rec[i*4+0]=(float)offs[i*2+0]; rec[i*4+1]=(float)offs[i*2+1]; } /* cone stays 0 */
        void *sp; CK(vkMapMemory(dev,smem,0,ssz,0,&sp)); memcpy(sp,rec,(size_t)nmesh*4*sizeof(float)); vkUnmapMemory(dev,smem); free(rec);
    }

    /* Descriptor set layout (binding 0 = SSBO). The task stage reads the scene when it
     * culls, and the compute builder writes it — include whichever stages exist. */
    VkShaderStageFlags sceneStages = VK_SHADER_STAGE_MESH_BIT_EXT
        | (VK_TASK_TRI_N>0 ? VK_SHADER_STAGE_TASK_BIT_EXT : 0)
        | (builder ? VK_SHADER_STAGE_COMPUTE_BIT : 0);
    VkDescriptorSetLayoutBinding dslb={.binding=0,.descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.descriptorCount=1,.stageFlags=sceneStages};
    VkDescriptorSetLayoutCreateInfo dslci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,.bindingCount=1,.pBindings=&dslb};
    VkDescriptorSetLayout dsl; CK(vkCreateDescriptorSetLayout(dev,&dslci,0,&dsl));
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.descriptorCount=1};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    VkDescriptorBufferInfo dbi={.buffer=ssbo,.offset=0,.range=ssz};
    VkWriteDescriptorSet wds={.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.pBufferInfo=&dbi};
    vkUpdateDescriptorSets(dev,1,&wds,0,0);

    /* Indirect command buffer: {groupCountX=N, 1, 1}. */
    VkDrawMeshTasksIndirectCommandEXT icmd={.groupCountX=nmesh,.groupCountY=1,.groupCountZ=1};
    VkBufferCreateInfo ibi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=sizeof icmd,.usage=VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT};
    VkBuffer ibuf; CK(vkCreateBuffer(dev,&ibi,0,&ibuf));
    VkMemoryRequirements imr; vkGetBufferMemoryRequirements(dev,ibuf,&imr);
    uint32_t imt=find_mem(pd,imr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(imt==~0u) return -1;
    VkMemoryAllocateInfo ima={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=imr.size,.memoryTypeIndex=imt};
    VkDeviceMemory imem; CK(vkAllocateMemory(dev,&ima,0,&imem)); vkBindBufferMemory(dev,ibuf,imem,0);
    void *ip; CK(vkMapMemory(dev,imem,0,sizeof icmd,0,&ip)); memcpy(ip,&icmd,sizeof icmd); vkUnmapMemory(dev,imem);

    /* Color target + framebuffer + pipeline (with the descriptor set layout). */
    VkImageCreateInfo ii={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage img; CK(vkCreateImage(dev,&ii,0,&img));
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,img,&mr);
    uint32_t it=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return -1;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    VkDeviceMemory im; CK(vkAllocateMemory(dev,&ma,0,&im)); vkBindImageMemory(dev,img,im,0);
    VkImageViewCreateInfo ivi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView view; CK(vkCreateImageView(dev,&ivi,0,&view));
    VkRenderPass rp=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL); if(!rp) return -1;
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&view,.width=W,.height=H,.layers=1};
    VkFramebuffer fb; CK(vkCreateFramebuffer(dev,&fbi,0,&fb));
    VkPipelineLayout pl; VkPipeline pipe=build_mesh_pipeline(dev,rp,W,H,&pl,dsl); if(!pipe) return -1;
    /* Compute meshlet builder pipeline (reuses the graphics layout — same set 0). */
    VkPipeline cpipe=0;
    if(builder){
        VkShaderModule cm=shmod(dev,VK_BUILD_COMP,VK_BUILD_COMP_N*4); if(!cm) return -1;
        VkComputePipelineCreateInfo cpci={.sType=VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
            .stage={.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_COMPUTE_BIT,.module=cm,.pName="main"},.layout=pl};
        vkCreateComputePipelines(dev,0,1,&cpci,0,&cpipe);
        vkDestroyShaderModule(dev,cm,0); if(!cpipe) return -1;
    }

    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=W*H*4,.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf; CK(vkCreateBuffer(dev,&bi,0,&buf));
    VkMemoryRequirements br; vkGetBufferMemoryRequirements(dev,buf,&br);
    uint32_t bt=find_mem(pd,br.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(bt==~0u) return -1;
    VkMemoryAllocateInfo bm={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=br.size,.memoryTypeIndex=bt};
    VkDeviceMemory bmem; CK(vkAllocateMemory(dev,&bm,0,&bmem)); vkBindBufferMemory(dev,buf,bmem,0);

    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; CK(vkAllocateCommandBuffers(dev,&cai,&cmd));
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&cbi);
    /* GPU meshlet build: fill the scene SSBO, then barrier so the draw sees it. */
    if(builder){
        vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,cpipe);
        vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&dset,0,0);
        vkCmdDispatch(cmd,nmesh,1,1);
        VkMemoryBarrier mb={.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER,.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT,.dstAccessMask=VK_ACCESS_SHADER_READ_BIT};
        vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_ALL_GRAPHICS_BIT,0,1,&mb,0,0,0,0);
    }
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl,0,1,&dset,0,0);
    VkShaderStageFlags pcStages = (VK_TASK_TRI_N>0) ? (VK_SHADER_STAGE_TASK_BIT_EXT|VK_SHADER_STAGE_MESH_BIT_EXT) : VK_SHADER_STAGE_MESH_BIT_EXT;
    vkCmdPushConstants(cmd,pl,pcStages,0,16,plane);   /* frustum plane for the @task cull */
    draw_indirect(cmd,ibuf,0,1,sizeof icmd);      /* N task/mesh workgroups, one indirect dispatch */
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int cy=(int)(H*0.52);
    unsigned char *L=&px[(cy*W + W/4)*4], *R=&px[(cy*W + 3*W/4)*4];
    int64_t mask=0;
    if(L[0]>40 || L[1]>40 || L[2]>40) mask|=1;       /* left quarter drawn */
    if(R[0]>40 || R[1]>40 || R[2]>40) mask|=2;       /* right quarter drawn */
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,ibuf,0); vkFreeMemory(dev,imem,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroyBuffer(dev,ssbo,0); vkFreeMemory(dev,smem,0);
    if(cpipe) vkDestroyPipeline(dev,cpipe,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0); vkDestroyFramebuffer(dev,fb,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return mask;
}

/* vk_mesh_scene(offsets): many meshlets, no culling — a permissive plane. */
int64_t jrt_vk_mesh_scene(const double *offs, int64_t nfloats) {
    float permissive[4]={0.0f,0.0f,0.0f,1.0f};
    return scene_render(offs,nfloats,permissive,0,0);
}

/* vk_mesh_scene_cull(offsets, nx,ny,nz,d): the fused GPU-driven cull renderer. The
 * @task tests each meshlet's center against the pushed frustum plane and emits only
 * the survivors (payload carries the index); the @mesh draws them. */
int64_t jrt_vk_mesh_scene_cull(const double *offs, int64_t nfloats, double nx, double ny, double nz, double dd) {
    float plane[4]={(float)nx,(float)ny,(float)nz,(float)dd};
    return scene_render(offs,nfloats,plane,0,0);
}

/* vk_mesh_built(count, nx,ny,nz,d): the whole renderer is GPU-built. A @compute
 * builder fills the scene SSBO with `count` meshlets on the GPU (set_meshlet), then
 * the @task cull + @mesh draw run over it — the meshlet set never exists on the host.
 * Returns the same left|right coverage mask. */
int64_t jrt_vk_mesh_built(int64_t count, double nx, double ny, double nz, double dd) {
    if(count <= 0) return -1;
    float plane[4]={(float)nx,(float)ny,(float)nz,(float)dd};
    return scene_render(0,0,plane,1,(uint32_t)count);
}

/* vk_mesh_shader(): the GPU-driven path (VM milestone). A mesh shader emits the
 * triangle itself — no vertex buffer, no vertex stage — dispatched with
 * vkCmdDrawMeshTasksEXT over VK_EXT_mesh_shader. Renders headless and returns the
 * centroid pixel (0xRRGGBB), or -2 if no device here supports mesh shaders (so the
 * caller/test can skip cleanly), or -1 on failure. The four args are a frustum plane
 * (nx,ny,nz,d) pushed as a 16-byte push constant for the @task cull. */
int64_t jrt_vk_mesh_shader(double px_, double py_, double pz_, double pw_) {
    enum { W=256, H=256 };
    float plane[4]={(float)px_,(float)py_,(float)pz_,(float)pw_};
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_3};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0); if(!nd) return -2;
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(inst,&nd,pds);
    VkPhysicalDevice pd=0; uint32_t qf=0;
    for(uint32_t d=0; d<nd && !pd; d++) {           /* pick a mesh-shader-capable device */
        if(!has_ext(pds[d], VK_EXT_MESH_SHADER_EXTENSION_NAME)) continue;
        uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,0);
        VkQueueFamilyProperties *qs=malloc(n*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,qs);
        for(uint32_t i=0;i<n;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){ pd=pds[d]; qf=i; break; }
        free(qs);
    }
    free(pds);
    if(!pd){ vkDestroyInstance(inst,0); return -2; }

    VkPhysicalDeviceMeshShaderFeaturesEXT mf={.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MESH_SHADER_FEATURES_EXT,.meshShader=VK_TRUE};
    const char *dext[]={VK_EXT_MESH_SHADER_EXTENSION_NAME};
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qf,.queueCount=1,.pQueuePriorities=&pr};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.pNext=&mf,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci,
        .enabledExtensionCount=1,.ppEnabledExtensionNames=dext};
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);
    PFN_vkCmdDrawMeshTasksEXT draw_mesh=(PFN_vkCmdDrawMeshTasksEXT)vkGetDeviceProcAddr(dev,"vkCmdDrawMeshTasksEXT");
    if(!draw_mesh){ vkDestroyDevice(dev,0); vkDestroyInstance(inst,0); return -2; }

    VkImageCreateInfo ii={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage img; CK(vkCreateImage(dev,&ii,0,&img));
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,img,&mr);
    uint32_t it=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return -1;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    VkDeviceMemory im; CK(vkAllocateMemory(dev,&ma,0,&im)); vkBindImageMemory(dev,img,im,0);
    VkImageViewCreateInfo ivi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView view; CK(vkCreateImageView(dev,&ivi,0,&view));
    VkRenderPass rp=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL); if(!rp) return -1;
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&view,.width=W,.height=H,.layers=1};
    VkFramebuffer fb; CK(vkCreateFramebuffer(dev,&fbi,0,&fb));
    VkPipelineLayout pl; VkPipeline pipe=build_mesh_pipeline(dev,rp,W,H,&pl,0); if(!pipe) return -1;

    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=W*H*4,.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf; CK(vkCreateBuffer(dev,&bi,0,&buf));
    VkMemoryRequirements br; vkGetBufferMemoryRequirements(dev,buf,&br);
    uint32_t bt=find_mem(pd,br.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(bt==~0u) return -1;
    VkMemoryAllocateInfo bm={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=br.size,.memoryTypeIndex=bt};
    VkDeviceMemory bmem; CK(vkAllocateMemory(dev,&bm,0,&bmem)); vkBindBufferMemory(dev,buf,bmem,0);

    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; CK(vkAllocateCommandBuffers(dev,&cai,&cmd));
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    VkShaderStageFlags pcStages = (VK_TASK_TRI_N>0) ? (VK_SHADER_STAGE_TASK_BIT_EXT|VK_SHADER_STAGE_MESH_BIT_EXT) : VK_SHADER_STAGE_MESH_BIT_EXT;
    vkCmdPushConstants(cmd,pl,pcStages,0,16,plane);   /* the frustum plane for @task cull */
    draw_mesh(cmd,1,1,1);                    /* one task workgroup → one meshlet */
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int cx=W/2, cy=(int)(H*0.55); unsigned char *c=&px[(cy*W+cx)*4], *tl=&px[(5*W+5)*4];
    int64_t packed = ((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    int corner_clear = tl[0]<60 && tl[1]<60 && tl[2]<60;
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0); vkDestroyFramebuffer(dev,fb,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return corner_clear ? packed : -1;
}

/* ---- windowed: open a window and present the triangle (frames=0: until closed) ---- */
int64_t jrt_vk_window(int64_t frames) {
    if(!glfwInit()) return 0;
    if(!glfwVulkanSupported()){ glfwTerminate(); return 0; }
    glfwWindowHint(GLFW_CLIENT_API, GLFW_NO_API);
    GLFWwindow *win=glfwCreateWindow(800,600,"Vire @vulkan — triangle",0,0);
    if(!win){ glfwTerminate(); return 0; }

    uint32_t next=0; const char **ext=glfwGetRequiredInstanceExtensions(&next);
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app,.enabledExtensionCount=next,.ppEnabledExtensionNames=ext};
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    VkSurfaceKHR surf; if(glfwCreateWindowSurface(inst,win,0,&surf)!=VK_SUCCESS) return 0;

    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0); if(!nd) return 0;
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(inst,&nd,pds);
    VkPhysicalDevice pd=0; uint32_t qf=0;
    for(uint32_t d=0; d<nd && !pd; d++) {
        uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,0);
        VkQueueFamilyProperties *qs=malloc(n*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pds[d],&n,qs);
        for(uint32_t i=0;i<n;i++){ VkBool32 present=0; vkGetPhysicalDeviceSurfaceSupportKHR(pds[d],i,surf,&present);
            if((qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT)&&present){ pd=pds[d]; qf=i; break; } }
        free(qs);
    }
    free(pds); if(!pd) return 0;
    const char *dext[]={VK_KHR_SWAPCHAIN_EXTENSION_NAME};
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qf,.queueCount=1,.pQueuePriorities=&pr};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci,.enabledExtensionCount=1,.ppEnabledExtensionNames=dext};
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);

    VkSurfaceCapabilitiesKHR caps; vkGetPhysicalDeviceSurfaceCapabilitiesKHR(pd,surf,&caps);
    uint32_t nf=0; vkGetPhysicalDeviceSurfaceFormatsKHR(pd,surf,&nf,0);
    VkSurfaceFormatKHR *fmts=malloc(nf*sizeof*fmts); vkGetPhysicalDeviceSurfaceFormatsKHR(pd,surf,&nf,fmts);
    VkSurfaceFormatKHR sf=fmts[0]; for(uint32_t i=0;i<nf;i++) if(fmts[i].format==VK_FORMAT_B8G8R8A8_UNORM){sf=fmts[i];break;} free(fmts);
    VkExtent2D ext2=caps.currentExtent;
    if(ext2.width==0xFFFFFFFFu){ /* Wayland: the surface size is ours to choose */
        int fw,fh; glfwGetFramebufferSize(win,&fw,&fh); ext2.width=(uint32_t)fw; ext2.height=(uint32_t)fh;
        if(ext2.width<caps.minImageExtent.width) ext2.width=caps.minImageExtent.width;
        if(ext2.height<caps.minImageExtent.height) ext2.height=caps.minImageExtent.height;
        if(ext2.width>caps.maxImageExtent.width) ext2.width=caps.maxImageExtent.width;
        if(ext2.height>caps.maxImageExtent.height) ext2.height=caps.maxImageExtent.height; }
    uint32_t W=ext2.width, H=ext2.height;
    uint32_t imgc=caps.minImageCount+1; if(caps.maxImageCount && imgc>caps.maxImageCount) imgc=caps.maxImageCount;
    VkSwapchainCreateInfoKHR sci={.sType=VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,.surface=surf,.minImageCount=imgc,
        .imageFormat=sf.format,.imageColorSpace=sf.colorSpace,.imageExtent=ext2,.imageArrayLayers=1,
        .imageUsage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,.imageSharingMode=VK_SHARING_MODE_EXCLUSIVE,
        .preTransform=caps.currentTransform,.compositeAlpha=VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        .presentMode=VK_PRESENT_MODE_FIFO_KHR,.clipped=VK_TRUE};
    VkSwapchainKHR sw; CK(vkCreateSwapchainKHR(dev,&sci,0,&sw));
    uint32_t nimg=0; vkGetSwapchainImagesKHR(dev,sw,&nimg,0);
    VkImage *imgs=malloc(nimg*sizeof*imgs); vkGetSwapchainImagesKHR(dev,sw,&nimg,imgs);
    VkRenderPass rp=build_rp(dev,sf.format,VK_IMAGE_LAYOUT_PRESENT_SRC_KHR); if(!rp) return 0;
    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl,0); if(!pipe) return 0;
    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,DEFAULT_TRI,6,&vbuf,&vmem)) return 0;

    VkImageView *views=malloc(nimg*sizeof*views); VkFramebuffer *fbs=malloc(nimg*sizeof*fbs);
    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBuffer *cmds=malloc(nimg*sizeof*cmds);
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=nimg};
    CK(vkAllocateCommandBuffers(dev,&cai,cmds));
    for(uint32_t i=0;i<nimg;i++){
        VkImageViewCreateInfo iv={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=imgs[i],.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=sf.format,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
        CK(vkCreateImageView(dev,&iv,0,&views[i]));
        VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&views[i],.width=W,.height=H,.layers=1};
        CK(vkCreateFramebuffer(dev,&fbi,0,&fbs[i]));
        rec_draw(cmds[i],rp,fbs[i],pipe,W,H,vbuf,3); CK(vkEndCommandBuffer(cmds[i]));
    }
    VkSemaphoreCreateInfo semi={.sType=VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,.flags=VK_FENCE_CREATE_SIGNALED_BIT};
    VkSemaphore avail,done; VkFence inflight;
    CK(vkCreateSemaphore(dev,&semi,0,&avail)); CK(vkCreateSemaphore(dev,&semi,0,&done)); CK(vkCreateFence(dev,&fci,0,&inflight));

    int64_t count=0;
    while(!glfwWindowShouldClose(win) && (frames==0 || count<frames)) {
        glfwPollEvents();
        vkWaitForFences(dev,1,&inflight,VK_TRUE,~0ull); vkResetFences(dev,1,&inflight);
        uint32_t idx=0; if(vkAcquireNextImageKHR(dev,sw,~0ull,avail,0,&idx)!=VK_SUCCESS) break;
        VkPipelineStageFlags wait=VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
        VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.waitSemaphoreCount=1,.pWaitSemaphores=&avail,.pWaitDstStageMask=&wait,
            .commandBufferCount=1,.pCommandBuffers=&cmds[idx],.signalSemaphoreCount=1,.pSignalSemaphores=&done};
        if(vkQueueSubmit(q,1,&si,inflight)!=VK_SUCCESS) break;
        VkPresentInfoKHR pi={.sType=VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,.waitSemaphoreCount=1,.pWaitSemaphores=&done,.swapchainCount=1,.pSwapchains=&sw,.pImageIndices=&idx};
        vkQueuePresentKHR(q,&pi);
        count++;
    }
    vkDeviceWaitIdle(dev);
    for(uint32_t i=0;i<nimg;i++){ vkDestroyFramebuffer(dev,fbs[i],0); vkDestroyImageView(dev,views[i],0); }
    vkDestroySemaphore(dev,avail,0); vkDestroySemaphore(dev,done,0); vkDestroyFence(dev,inflight,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyCommandPool(dev,cp,0); vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroySwapchainKHR(dev,sw,0); vkDestroyDevice(dev,0);
    vkDestroySurfaceKHR(inst,surf,0); vkDestroyInstance(inst,0);
    glfwDestroyWindow(win); glfwTerminate();
    free(imgs); free(views); free(fbs); free(cmds);
    return 1;
}
