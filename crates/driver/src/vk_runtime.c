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

/* Zero-cost validation gating (V5). Under `--debug` the Vire build compiles this TU
 * with -DFASTLLVM_VK_VALIDATE: `VK_CREATE_INSTANCE` enables the Khronos validation
 * layer + a `VK_EXT_debug_utils` messenger that prints API misuse to stderr, catching
 * Vulkan usage bugs at dev time. In release the macro is undefined and every line here
 * vanishes — `VK_CREATE_INSTANCE` is a bare `vkCreateInstance`, no layer, no messenger,
 * no cost. The messenger rides on the persistent instance (never destroyed), so it
 * needs no teardown. */
#ifdef FASTLLVM_VK_VALIDATE
static VkDebugUtilsMessengerEXT g_vk_messenger = 0;
static VKAPI_ATTR VkBool32 VKAPI_CALL vk_val_cb(
        VkDebugUtilsMessageSeverityFlagBitsEXT sev,
        VkDebugUtilsMessageTypeFlagsEXT type,
        const VkDebugUtilsMessengerCallbackDataEXT *data, void *ud) {
    (void)type; (void)ud;
    if (sev >= VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT)
        fprintf(stderr, "[vk-validation] %s\n", data && data->pMessage ? data->pMessage : "(null)");
    return VK_FALSE;
}
static VkResult vk_val_instance(VkInstanceCreateInfo *ici, VkInstance *out) {
    const char *exts[32];
    unsigned ne = 0;
    for (unsigned i = 0; i < ici->enabledExtensionCount && ne < 31; i++) exts[ne++] = ici->ppEnabledExtensionNames[i];
    exts[ne++] = "VK_EXT_debug_utils";
    static const char *layers[] = { "VK_LAYER_KHRONOS_validation" };
    ici->enabledLayerCount = 1;
    ici->ppEnabledLayerNames = layers;
    ici->enabledExtensionCount = ne;
    ici->ppEnabledExtensionNames = exts;
    VkResult r = vkCreateInstance(ici, 0, out);
    if (r != VK_SUCCESS) return r;
    PFN_vkCreateDebugUtilsMessengerEXT mk =
        (PFN_vkCreateDebugUtilsMessengerEXT)vkGetInstanceProcAddr(*out, "vkCreateDebugUtilsMessengerEXT");
    if (mk) {
        VkDebugUtilsMessengerCreateInfoEXT mi = {
            .sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
            .messageSeverity = VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT,
            .messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT,
            .pfnUserCallback = vk_val_cb,
        };
        mk(*out, &mi, 0, &g_vk_messenger);
    }
    return VK_SUCCESS;
}
#define VK_CREATE_INSTANCE(ici, out) vk_val_instance((ici), (out))
#else
#define VK_CREATE_INSTANCE(ici, out) vkCreateInstance((ici), 0, (out))
#endif

/* Shader SPIR-V is generated at Vire build time (crates/backend/src/spirv.rs ->
 * spirv-as) into vk_shaders.c and linked alongside — the @fragment color comes
 * from the Vire source. Declared extern here (word counts as *_N). */
extern const uint32_t VK_TRI_VERT[]; extern const unsigned VK_TRI_VERT_N;
extern const uint32_t VK_TRI_FRAG[]; extern const unsigned VK_TRI_FRAG_N;
extern const uint32_t VK_MESH_TRI[]; extern const unsigned VK_MESH_TRI_N;
extern const uint32_t VK_TASK_TRI[]; extern const unsigned VK_TASK_TRI_N; /* N=0 → no task stage */
extern const uint32_t VK_BUILD_COMP[]; extern const unsigned VK_BUILD_COMP_N; /* N=0 → no compute builder */
extern const uint32_t VK_GPUVK_COMP[]; extern const unsigned VK_GPUVK_COMP_N; /* N=0 → no @gpuvk map */
extern const uint32_t VK_PASS1_FRAG[]; extern const unsigned VK_PASS1_FRAG_N; /* fixed red fragment for pass 1 */
extern const uint32_t VK_PASS2_FRAG[]; extern const unsigned VK_PASS2_FRAG_N; /* fixed blue fragment (source B) */

/* V3: the descriptor/push interface REFLECTED from the shader stages' resource usage
 * (crates/vire/src/shader.rs → main.rs emits these). The descriptor-set + pipeline
 * layout are built from this instead of a hardcoded per-demo layout. Flat parallel
 * arrays. KIND: 0 = storage buffer, 1 = combined image sampler. STAGES is a
 * VkShaderStageFlags bitmask (the frontend's bits equal Vulkan's). */
extern const unsigned VK_IFACE_NB;
extern const unsigned VK_IFACE_BINDING[];
extern const unsigned VK_IFACE_KIND[];
extern const unsigned VK_IFACE_STAGES[];
extern const unsigned VK_IFACE_PUSH_SIZE;
extern const unsigned VK_IFACE_PUSH_STAGES;

/* Build the VkDescriptorSetLayout for descriptor set 0 from the reflected interface.
 * Returns 0 (and creates nothing) when the shader declares no bindings — callers that
 * genuinely need a set treat 0 as "no descriptors", exactly like the old dsl==0 path. */
static VkDescriptorSetLayout mk_dsl_reflected(VkDevice dev) {
    if (VK_IFACE_NB == 0) return 0;
    VkDescriptorSetLayoutBinding b[16];
    unsigned n = VK_IFACE_NB < 16 ? VK_IFACE_NB : 16;
    for (unsigned i = 0; i < n; i++) {
        b[i].binding = VK_IFACE_BINDING[i];
        b[i].descriptorType = VK_IFACE_KIND[i] == 1 ? VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER
                                                    : VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
        b[i].descriptorCount = 1;
        b[i].stageFlags = VK_IFACE_STAGES[i];
        b[i].pImmutableSamplers = 0;
    }
    VkDescriptorSetLayoutCreateInfo ci = {.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount=n, .pBindings=b};
    VkDescriptorSetLayout dsl;
    if (vkCreateDescriptorSetLayout(dev,&ci,0,&dsl)!=VK_SUCCESS) return 0;
    return dsl;
}

/* Build a VkPipelineLayout from a descriptor-set layout (may be 0) plus the REFLECTED
 * push-constant range (VK_IFACE_PUSH_SIZE/STAGES from the shader). push_size == 0 → no
 * range. Together with mk_dsl_reflected() this makes the whole pipeline layout —
 * descriptors AND push — come from the shader signatures rather than a hardcoded range.
 * (The graphics vertex/fragment pipeline keeps its fixed 16-byte per-frame `uniform()`
 * channel; this reflected path is for the mesh/task stages whose push IS the shader's
 * `cull_plane()`.) Returns 1 on success. */
static int mk_pipeline_layout_reflected(VkDevice dev, VkDescriptorSetLayout dsl, VkPipelineLayout *out) {
    VkPushConstantRange pcr={.stageFlags=VK_IFACE_PUSH_STAGES,.offset=0,.size=VK_IFACE_PUSH_SIZE};
    VkPipelineLayoutCreateInfo plci={.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount=(dsl?1u:0u),.pSetLayouts=(dsl?&dsl:0),
        .pushConstantRangeCount=(VK_IFACE_PUSH_SIZE?1u:0u),
        .pPushConstantRanges=(VK_IFACE_PUSH_SIZE?&pcr:0)};
    return vkCreatePipelineLayout(dev,&plci,0,out)==VK_SUCCESS;
}

/* Insert the correct pipeline barrier for an image layout transition — the render
 * graph's "minimal correct barriers": src/dst access masks + pipeline stages are
 * derived from (old,new) layouts, so callers don't hand-tune them. Covers the
 * transitions the two-pass render needs (attachment write → shader read, etc.). */
static void auto_barrier(VkCommandBuffer cmd, VkImage img, VkImageLayout oldL, VkImageLayout newL) {
    VkAccessFlags src=0, dst=0; VkPipelineStageFlags ss=VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, ds=VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT;
    if(oldL==VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL){ src=VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT; ss=VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT; }
    else if(oldL==VK_IMAGE_LAYOUT_PREINITIALIZED){ src=VK_ACCESS_HOST_WRITE_BIT; ss=VK_PIPELINE_STAGE_HOST_BIT; }
    if(newL==VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL){ dst=VK_ACCESS_SHADER_READ_BIT; ds=VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT; }
    else if(newL==VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL){ dst=VK_ACCESS_TRANSFER_READ_BIT; ds=VK_PIPELINE_STAGE_TRANSFER_BIT; }
    VkImageMemoryBarrier b={.sType=VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,.srcAccessMask=src,.dstAccessMask=dst,
        .oldLayout=oldL,.newLayout=newL,.image=img,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    vkCmdPipelineBarrier(cmd,ss,ds,0,0,0,0,0,1,&b);
}

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
    VkInstance inst; CK(VK_CREATE_INSTANCE(&ici,&inst));
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
static VkPipeline build_pipeline_d(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, int colored, VkDescriptorSetLayout dsl, const uint32_t *fcode, unsigned fn, int depth);
static VkPipeline build_pipeline_f(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, int colored, VkDescriptorSetLayout dsl, const uint32_t *fcode, unsigned fn) {
    return build_pipeline_d(dev,rp,w,h,out_layout,colored,dsl,fcode,fn,0);
}
static VkPipeline build_pipeline_d(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, int colored, VkDescriptorSetLayout dsl, const uint32_t *fcode, unsigned fn, int depth) {
    VkShaderModule vs=shmod(dev,VK_TRI_VERT,VK_TRI_VERT_N*4), fs=shmod(dev,fcode,fn*4);
    if(!vs||!fs) return 0;
    VkPipelineShaderStageCreateInfo st[2]={
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_VERTEX_BIT,.module=vs,.pName="main"},
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_FRAGMENT_BIT,.module=fs,.pName="main"}};
    /* Vertex buffer at binding 0. `colored` selects the layout: 0 = position-only
     * (x,y) f32; 1 = 2D + per-vertex color (x,y, r,g,b) as vk_mesh_c; 2 = 3D + color
     * (x,y,z, r,g,b) for GPU-side 3D — Location 0 becomes a vec3, and back-face culling
     * is enabled so a convex mesh (e.g. the sphere) hides its far side without a depth
     * buffer. Position at Location 0, color at Location 1 (offset = position size). */
    int is3d = (colored==2); int hascol = (colored>=1);
    unsigned posn = is3d?3u:2u; unsigned stride = posn + (hascol?3u:0u);
    VkVertexInputBindingDescription vbind={.binding=0,.stride=stride*sizeof(float),.inputRate=VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription vattr[2]={
        {.location=0,.binding=0,.format=(is3d?VK_FORMAT_R32G32B32_SFLOAT:VK_FORMAT_R32G32_SFLOAT),.offset=0},
        {.location=1,.binding=0,.format=VK_FORMAT_R32G32B32_SFLOAT,.offset=posn*sizeof(float)}};
    VkPipelineVertexInputStateCreateInfo vi={.sType=VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount=1,.pVertexBindingDescriptions=&vbind,
        .vertexAttributeDescriptionCount=(uint32_t)(hascol?2:1),.pVertexAttributeDescriptions=vattr};
    VkPipelineInputAssemblyStateCreateInfo ia={.sType=VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport vp={0,0,(float)w,(float)h,0,1}; VkRect2D sc={{0,0},{w,h}};
    VkPipelineViewportStateCreateInfo vps={.sType=VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,.viewportCount=1,.pViewports=&vp,.scissorCount=1,.pScissors=&sc};
    VkPipelineRasterizationStateCreateInfo rs={.sType=VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,.polygonMode=VK_POLYGON_MODE_FILL,.cullMode=(is3d?VK_CULL_MODE_BACK_BIT:VK_CULL_MODE_NONE),.frontFace=VK_FRONT_FACE_COUNTER_CLOCKWISE,.lineWidth=1.0f};
    VkPipelineMultisampleStateCreateInfo ms={.sType=VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState cba={.colorWriteMask=0xf};
    VkPipelineColorBlendStateCreateInfo cb={.sType=VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,.attachmentCount=1,.pAttachments=&cba};
    /* A 16-byte push constant (vec4) for `uniform()` in the vertex/fragment stages. */
    VkPushConstantRange pcr={.stageFlags=VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,.offset=0,.size=16};
    VkPipelineLayoutCreateInfo plci={.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,.pushConstantRangeCount=1,.pPushConstantRanges=&pcr,
        .setLayoutCount=(dsl?1u:0u),.pSetLayouts=(dsl?&dsl:0)};
    if(vkCreatePipelineLayout(dev,&plci,0,out_layout)!=VK_SUCCESS) return 0;
    /* Optional depth test (for general 3D where back-face culling isn't enough). */
    VkPipelineDepthStencilStateCreateInfo ds={.sType=VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        .depthTestEnable=VK_TRUE,.depthWriteEnable=VK_TRUE,.depthCompareOp=VK_COMPARE_OP_LESS};
    VkGraphicsPipelineCreateInfo gp={.sType=VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,.stageCount=2,.pStages=st,
        .pVertexInputState=&vi,.pInputAssemblyState=&ia,.pViewportState=&vps,.pRasterizationState=&rs,
        .pMultisampleState=&ms,.pColorBlendState=&cb,.pDepthStencilState=(depth?&ds:0),.layout=*out_layout,.renderPass=rp,.subpass=0};
    VkPipeline pipe=0; vkCreateGraphicsPipelines(dev,0,1,&gp,0,&pipe);
    vkDestroyShaderModule(dev,vs,0); vkDestroyShaderModule(dev,fs,0);
    return pipe;
}
/* The default triangle pipeline uses the program's @fragment (VK_TRI_FRAG). */
static VkPipeline build_pipeline(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, int colored, VkDescriptorSetLayout dsl) {
    return build_pipeline_f(dev,rp,w,h,out_layout,colored,dsl,VK_TRI_FRAG,VK_TRI_FRAG_N);
}
/* The GPU-driven pipeline: a mesh stage (no vertex input, no input assembly — the
 * mesh shader emits vertices + primitives itself) + the fragment stage. */
static VkPipeline build_mesh_pipeline(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout, VkDescriptorSetLayout dsl, int depth) {
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
    /* Pipeline layout — descriptor set AND the push-constant range (the frustum plane
     * for the @task `cull_plane()`) both REFLECTED from the shader interface, so the
     * range's size and stage mask come from which stage actually reads the push. */
    if(!mk_pipeline_layout_reflected(dev, dsl, out_layout)) return 0;
    VkPipelineDepthStencilStateCreateInfo ds={.sType=VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        .depthTestEnable=VK_TRUE,.depthWriteEnable=VK_TRUE,.depthCompareOp=VK_COMPARE_OP_LESS};
    VkGraphicsPipelineCreateInfo gp={.sType=VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,.stageCount=nst,.pStages=st,
        .pViewportState=&vps,.pRasterizationState=&rs,.pMultisampleState=&msi,.pColorBlendState=&cb,
        .pDepthStencilState=depth?&ds:0,
        .layout=*out_layout,.renderPass=rp,.subpass=0};
    VkPipeline pipe=0; vkCreateGraphicsPipelines(dev,0,1,&gp,0,&pipe);
    vkDestroyShaderModule(dev,ms,0); vkDestroyShaderModule(dev,fs,0); if(ts) vkDestroyShaderModule(dev,ts,0);
    return pipe;
}
#define DEPTH_FMT VK_FORMAT_D32_SFLOAT
static VkRenderPass build_rp_d(VkDevice dev, VkFormat fmt, VkImageLayout final, int depth) {
    VkAttachmentDescription att[2]={
        {.format=fmt,.samples=VK_SAMPLE_COUNT_1_BIT,
         .loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR,.storeOp=VK_ATTACHMENT_STORE_OP_STORE,
         .stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE,.stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE,
         .initialLayout=VK_IMAGE_LAYOUT_UNDEFINED,.finalLayout=final},
        {.format=DEPTH_FMT,.samples=VK_SAMPLE_COUNT_1_BIT,
         .loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR,.storeOp=VK_ATTACHMENT_STORE_OP_DONT_CARE,
         .stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE,.stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE,
         .initialLayout=VK_IMAGE_LAYOUT_UNDEFINED,.finalLayout=VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL}};
    VkAttachmentReference ref={0,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkAttachmentReference dref={1,VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub={.pipelineBindPoint=VK_PIPELINE_BIND_POINT_GRAPHICS,.colorAttachmentCount=1,.pColorAttachments=&ref,
        .pDepthStencilAttachment=depth?&dref:0};
    VkSubpassDependency dep={.srcSubpass=VK_SUBPASS_EXTERNAL,.dstSubpass=0,
        .srcStageMask=VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,.dstStageMask=VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
        .dstAccessMask=VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT};
    VkRenderPassCreateInfo ci={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,.attachmentCount=(uint32_t)(depth?2:1),.pAttachments=att,.subpassCount=1,.pSubpasses=&sub,.dependencyCount=1,.pDependencies=&dep};
    VkRenderPass rp=0; vkCreateRenderPass(dev,&ci,0,&rp); return rp;
}
static VkRenderPass build_rp(VkDevice dev, VkFormat fmt, VkImageLayout final) { return build_rp_d(dev,fmt,final,0); }
static void rec_draw_d(VkCommandBuffer cmd, VkRenderPass rp, VkFramebuffer fb, VkPipeline pipe,
                     uint32_t w, uint32_t h, VkBuffer vbuf, uint32_t nverts,
                     VkPipelineLayout pl, const float uni[4], int depth) {
    VkCommandBufferBeginInfo bi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&bi);
    VkClearValue clear[2]={{.color={{0.08f,0.08f,0.10f,1.0f}}},{.depthStencil={1.0f,0}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{w,h}},.clearValueCount=(uint32_t)(depth?2:1),.pClearValues=clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    float zero[4]={0,0,0,0};
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,uni?uni:zero); /* uniform() */
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off);
    vkCmdDraw(cmd,nverts,1,0,0);
    vkCmdEndRenderPass(cmd);
}
static void rec_draw(VkCommandBuffer cmd, VkRenderPass rp, VkFramebuffer fb, VkPipeline pipe,
                     uint32_t w, uint32_t h, VkBuffer vbuf, uint32_t nverts,
                     VkPipelineLayout pl, const float uni[4]) {
    rec_draw_d(cmd,rp,fb,pipe,w,h,vbuf,nverts,pl,uni,0);
}

/* ---- headless: render `nverts` triangle-list vertices (interleaved f32 x,y) to an
 *      image, read back; returns the centroid pixel packed as 0xRRGGBB (so callers
 *      can check the @fragment color), or -1 on failure ---- */
/* One persistent Vulkan context, shared by every render (headless graphics AND the mesh/
 * meshlet path). Created once, lazily; never destroyed — so GPU resources (textures,
 * buffers, the meshlet scene) outlive a call and a program pays the device-creation cost
 * once instead of per frame. `g_has_mesh` records whether the device could enable
 * VK_EXT_mesh_shader; `g_draw_mesh` is its indirect-draw entry point. */
static VkInstance g_inst; static VkPhysicalDevice g_pd; static VkDevice g_dev; static VkQueue g_gq; static uint32_t g_gqf; static int g_ctx_ok=0;
static int g_has_mesh=0;
static PFN_vkCmdDrawMeshTasksIndirectEXT g_draw_mesh=0;
/* The persistent context + pipeline cache are shared mutable state, and the render queue
 * (g_gq) is not externally synchronised — so `vk_*` rendering is SINGLE-THREADED. Under
 * threads a lock serialises context init + cache access + submission (correctness over
 * concurrency; GPU renders serialise anyway). Without threads it compiles to nothing. */
#if defined(FASTLLVM_THREADS)
#include <pthread.h>
static pthread_mutex_t g_vk_lock = PTHREAD_MUTEX_INITIALIZER;
#define VK_LOCK()   pthread_mutex_lock(&g_vk_lock)
#define VK_UNLOCK() pthread_mutex_unlock(&g_vk_lock)
#else
#define VK_LOCK()   ((void)0)
#define VK_UNLOCK() ((void)0)
#endif
/* FNV-1a over a shader's SPIR-V words — folded into the pipeline-cache key so two
 * different shaders can never collide on the same (mode,depth,size) slot. */
static uint64_t spv_hash(const uint32_t *code, unsigned n) {
    uint64_t h=1469598103934665603ull;
    for(unsigned i=0;i<n;i++){ h^=code[i]; h*=1099511628211ull; }
    return h;
}
/* Locking wrapper: first init wins, others wait, everyone sees the ready context. */
static int ctx_init_unlocked(void);
static int ctx_init(void) {
    if(g_ctx_ok) return 1;                 /* fast path — already initialised */
    VK_LOCK();
    int r = g_ctx_ok ? 1 : ctx_init_unlocked();
    VK_UNLOCK();
    return r;
}
static int ctx_init_unlocked(void) {
    if(g_ctx_ok) return 1;
    /* API 1.3 instance so VK_EXT_mesh_shader (a 1.3-era device extension) is usable. */
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_3};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    if(VK_CREATE_INSTANCE(&ici,&g_inst)!=VK_SUCCESS) return 0;
    uint32_t nd=0; vkEnumeratePhysicalDevices(g_inst,&nd,0); if(!nd) return 0;
    VkPhysicalDevice *pds=malloc(nd*sizeof*pds); vkEnumeratePhysicalDevices(g_inst,&nd,pds);
    /* Keep the FIRST graphics device as the persistent context (stable — on a multi-GPU
     * box, switching the graphics context to a different vendor can hit vendor-specific
     * driver bugs). Enable mesh shaders only if THIS device supports them; if it doesn't,
     * g_has_mesh stays 0 and the mesh path keeps its own device (no regression). */
    g_pd=pds[0]; free(pds); int f=0;
    { uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(g_pd,&n,0);
      VkQueueFamilyProperties *qs=malloc(n*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(g_pd,&n,qs);
      for(uint32_t i=0;i<n;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){g_gqf=i;f=1;break;} free(qs); }
    if(!f) return 0;
    g_has_mesh = has_ext(g_pd, VK_EXT_MESH_SHADER_EXTENSION_NAME);
    float pr=1; VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=g_gqf,.queueCount=1,.pQueuePriorities=&pr};
    VkPhysicalDeviceMeshShaderFeaturesEXT mf={.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MESH_SHADER_FEATURES_EXT,.meshShader=VK_TRUE,.taskShader=VK_TRUE};
    const char *dext[]={VK_EXT_MESH_SHADER_EXTENSION_NAME};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci,
        .pNext=(g_has_mesh?&mf:0),.enabledExtensionCount=(g_has_mesh?1u:0u),.ppEnabledExtensionNames=(g_has_mesh?dext:0)};
    if(vkCreateDevice(g_pd,&dci,0,&g_dev)!=VK_SUCCESS){
        /* mesh-device creation can fail (partial support) → retry plain graphics. */
        if(g_has_mesh){ g_has_mesh=0; VkDeviceCreateInfo pdci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci};
            if(vkCreateDevice(g_pd,&pdci,0,&g_dev)!=VK_SUCCESS) return 0; }
        else return 0;
    }
    vkGetDeviceQueue(g_dev,g_gqf,0,&g_gq);
    if(g_has_mesh) g_draw_mesh=(PFN_vkCmdDrawMeshTasksIndirectEXT)vkGetDeviceProcAddr(g_dev,"vkCmdDrawMeshTasksIndirectEXT");
    g_ctx_ok=1; return 1;
}

/* Cache of the expensive, reusable render objects — the render pass, the pipeline and
 * its layout — keyed by (vertex mode, depth, W, H). Building a pipeline compiles the
 * SPIR-V; an animation that renders the same shaders every frame must not pay that per
 * frame. Lives on the persistent context, so it is built once and reused. */
typedef struct { int valid; int vmode, depth; uint32_t w, h; uint64_t shash; VkRenderPass rp; VkPipeline pipe; VkPipelineLayout pl; } RCacheEntry;
static RCacheEntry g_rcache[8];
static int rcache_get(VkDevice dev, int vmode, int depth, uint32_t w, uint32_t h,
                      VkRenderPass *rp, VkPipeline *pipe, VkPipelineLayout *pl) {
    /* The shader IS part of the key (hashed), so two @vertex/@fragment pairs can never
     * share a slot even at the same mode/depth/size — a wrong-pipeline hit is impossible. */
    uint64_t sh = spv_hash(VK_TRI_VERT,VK_TRI_VERT_N) ^ (spv_hash(VK_TRI_FRAG,VK_TRI_FRAG_N)*3);
    VK_LOCK();   /* the cache map is shared mutable state — serialise access */
    for(int i=0;i<8;i++){ RCacheEntry *e=&g_rcache[i];
        if(e->valid && e->vmode==vmode && e->depth==depth && e->w==w && e->h==h && e->shash==sh){ *rp=e->rp; *pipe=e->pipe; *pl=e->pl; VK_UNLOCK(); return 1; } }
    VkRenderPass r = depth ? build_rp_d(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,1)
                           : build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
    if(!r){ VK_UNLOCK(); return 0; }
    VkPipelineLayout layout; VkPipeline p=build_pipeline_d(dev,r,w,h,&layout,vmode,0,VK_TRI_FRAG,VK_TRI_FRAG_N,depth);
    if(!p){ vkDestroyRenderPass(dev,r,0); VK_UNLOCK(); return 0; }
    for(int i=0;i<8;i++){ RCacheEntry *e=&g_rcache[i]; if(!e->valid){ *e=(RCacheEntry){1,vmode,depth,w,h,sh,r,p,layout}; break; } }
    *rp=r; *pipe=p; *pl=layout; VK_UNLOCK(); return 1;
}

/* Persistent per-frame render resources — the color/depth images + views, framebuffer, the
 * readback and vertex staging buffers, the command pool/buffer and the fence. Creating these
 * per frame costs ~0.1ms/frame (measured: 200-frame animation spent ~21ms of ~197ms here);
 * an animation reuses ONE entry keyed by (W,H,depth) so per-frame work drops to a vertex
 * memcpy + command re-record + submit. The readback and vertex buffers stay persistently
 * mapped (host-coherent). Lives on the persistent context, like g_rcache.
 * Single-render-at-a-time: entries hold mutable per-frame state (command buffer, vertex
 * buffer), so concurrent same-key renders are NOT supported — Vire drives vk_draw serially. */
/* One in-flight frame's resources. With a ring depth > 1 (FASTLLVM_VK_PIPE=n / vk_pipeline_
 * depth), several frames are on the GPU at once, so EACH needs its own color/depth image +
 * framebuffer (two frames must not render into the same image) and its own cmd/fence/readback. */
typedef struct {
    VkImage img; VkDeviceMemory im; VkImageView view;
    VkImage dimg; VkDeviceMemory dmem; VkImageView dview;
    VkFramebuffer fb;
    VkBuffer sbuf; VkDeviceMemory smem; unsigned char *px;   /* readback, persistently mapped */
    VkBuffer vbuf; VkDeviceMemory vmem; void *vmap; uint32_t vcap; /* vertex, grow-only, mapped */
    VkCommandPool cp; VkCommandBuffer cmd; VkFence fence;
    int inflight;           /* a submit is pending on this slot's fence */
    char ppm[512];          /* deferred: the PPM path to write when this frame is harvested */
} FSlot;
#define FRING_MAX 6
typedef struct {
    int valid; uint32_t w,h; int depth; int ring; long seq;
    FSlot s[FRING_MAX];
} FCacheEntry;
static FCacheEntry g_fcache[8];
/* Pipeline depth: 1 = classic synchronous (wait each frame, return THIS frame — the sound
 * default). n>1 = keep n frames in flight; vk_draw then returns the pixel from n-1 frames ago
 * and the trailing frames flush at exit — an OPT-IN for animations that ignore the return
 * (e.g. PPM output). Set via env FASTLLVM_VK_PIPE or vk_pipeline_depth(n). */
static int g_pipe_depth=1;

/* Grow-only host-visible vertex buffer inside a slot — (re)creates only when it must grow. */
static int fc_ensure_vbuf(VkDevice dev, VkPhysicalDevice pd, FSlot *e, uint32_t nfloats){
    if(e->vbuf && e->vcap>=nfloats) return 1;
    if(e->vbuf){ vkUnmapMemory(dev,e->vmem); vkDestroyBuffer(dev,e->vbuf,0); vkFreeMemory(dev,e->vmem,0); e->vbuf=0; e->vmem=0; e->vmap=0; }
    uint32_t cap = nfloats?nfloats:2;
    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=(VkDeviceSize)cap*sizeof(float),.usage=VK_BUFFER_USAGE_VERTEX_BUFFER_BIT};
    if(vkCreateBuffer(dev,&bi,0,&e->vbuf)!=VK_SUCCESS) return 0;
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,e->vbuf,&mr);
    uint32_t t=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(t==~0u) return 0;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=t};
    if(vkAllocateMemory(dev,&ma,0,&e->vmem)!=VK_SUCCESS) return 0;
    vkBindBufferMemory(dev,e->vbuf,e->vmem,0);
    if(vkMapMemory(dev,e->vmem,0,VK_WHOLE_SIZE,0,&e->vmap)!=VK_SUCCESS) return 0;
    e->vcap=cap; return 1;
}

/* Build one slot's full resource set for (W,H,depth). rp comes from rcache. */
static int fslot_build(VkDevice dev, VkPhysicalDevice pd, uint32_t qf, int depth, uint32_t W, uint32_t H, VkRenderPass rp, uint32_t nfloats, FSlot *e){
    memset(e,0,sizeof(*e));
    VkImageCreateInfo ii={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    if(vkCreateImage(dev,&ii,0,&e->img)!=VK_SUCCESS) return 0;
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,e->img,&mr);
    uint32_t it=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return 0;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    if(vkAllocateMemory(dev,&ma,0,&e->im)!=VK_SUCCESS) return 0; vkBindImageMemory(dev,e->img,e->im,0);
    VkImageViewCreateInfo vi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=e->img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    if(vkCreateImageView(dev,&vi,0,&e->view)!=VK_SUCCESS) return 0;
    if(depth){
        VkImageCreateInfo di={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_D32_SFLOAT,
            .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
            .usage=VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
        if(vkCreateImage(dev,&di,0,&e->dimg)!=VK_SUCCESS) return 0;
        VkMemoryRequirements dmr; vkGetImageMemoryRequirements(dev,e->dimg,&dmr);
        uint32_t dt=find_mem(pd,dmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(dt==~0u) return 0;
        VkMemoryAllocateInfo dma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=dmr.size,.memoryTypeIndex=dt};
        if(vkAllocateMemory(dev,&dma,0,&e->dmem)!=VK_SUCCESS) return 0; vkBindImageMemory(dev,e->dimg,e->dmem,0);
        VkImageViewCreateInfo dvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=e->dimg,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_D32_SFLOAT,.subresourceRange={VK_IMAGE_ASPECT_DEPTH_BIT,0,1,0,1}};
        if(vkCreateImageView(dev,&dvi,0,&e->dview)!=VK_SUCCESS) return 0;
    }
    VkImageView atts[2]={e->view,e->dview};
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=(uint32_t)(depth?2:1),.pAttachments=atts,.width=W,.height=H,.layers=1};
    if(vkCreateFramebuffer(dev,&fbi,0,&e->fb)!=VK_SUCCESS) return 0;
    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=(VkDeviceSize)W*H*4,.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    if(vkCreateBuffer(dev,&bi,0,&e->sbuf)!=VK_SUCCESS) return 0;
    VkMemoryRequirements br; vkGetBufferMemoryRequirements(dev,e->sbuf,&br);
    uint32_t bt=find_mem(pd,br.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(bt==~0u) return 0;
    VkMemoryAllocateInfo bm={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=br.size,.memoryTypeIndex=bt};
    if(vkAllocateMemory(dev,&bm,0,&e->smem)!=VK_SUCCESS) return 0; vkBindBufferMemory(dev,e->sbuf,e->smem,0);
    if(vkMapMemory(dev,e->smem,0,(VkDeviceSize)W*H*4,0,(void**)&e->px)!=VK_SUCCESS) return 0;
    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,.queueFamilyIndex=qf};
    if(vkCreateCommandPool(dev,&cpi,0,&e->cp)!=VK_SUCCESS) return 0;
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=e->cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    if(vkAllocateCommandBuffers(dev,&cai,&e->cmd)!=VK_SUCCESS) return 0;
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    if(vkCreateFence(dev,&fci,0,&e->fence)!=VK_SUCCESS) return 0;
    if(!fc_ensure_vbuf(dev,pd,e,nfloats)) return 0;
    return 1;
}

/* Fetch (or build) the ring of persistent resources for (W,H,depth); ensures the ACTIVE slot's
 * vertex buffer holds >= nfloats. rp comes from rcache (depends only on depth here), so a fixed-
 * depth framebuffer stays render-pass-compatible across vmodes. Returns NULL on failure or when
 * all 8 keys are taken (unrealistic — matches g_rcache's fixed slot count). */
static FCacheEntry* fcache_get(VkDevice dev, VkPhysicalDevice pd, uint32_t qf, int depth, uint32_t W, uint32_t H, VkRenderPass rp, uint32_t nfloats){
    VK_LOCK();
    FCacheEntry *e=0;
    for(int i=0;i<8;i++){ FCacheEntry *c=&g_fcache[i]; if(c->valid && c->w==W && c->h==H && c->depth==depth){ e=c; break; } }
    if(e){ int idx=(int)(e->seq % e->ring); int ok=fc_ensure_vbuf(dev,pd,&e->s[idx],nfloats); VK_UNLOCK(); return ok?e:0; }
    for(int i=0;i<8;i++){ if(!g_fcache[i].valid){ e=&g_fcache[i]; break; } }
    if(!e){ VK_UNLOCK(); return 0; }
    memset(e,0,sizeof(*e));
    int ring = g_pipe_depth<1?1:(g_pipe_depth>FRING_MAX?FRING_MAX:g_pipe_depth);
    for(int i=0;i<ring;i++){ if(!fslot_build(dev,pd,qf,depth,W,H,rp,nfloats,&e->s[i])){ VK_UNLOCK(); return 0; } }
    e->valid=1; e->w=W; e->h=H; e->depth=depth; e->ring=ring; e->seq=0;
    VK_UNLOCK(); return e;
}

/* Read one slot's readback (after its fence has signalled) into the packed-centroid result the
 * caller expects, and write its deferred PPM if one is pending. */
static int64_t fslot_harvest(uint32_t W, uint32_t H, FSlot *sl){
    unsigned char *px = sl->px;
    if(sl->ppm[0]){
        FILE *fp=fopen(sl->ppm,"wb");
        if(fp){ fprintf(fp,"P6\n%u %u\n255\n",W,H); for(uint32_t i=0;i<W*H;i++) fwrite(&px[i*4],1,3,fp); fclose(fp); }
        sl->ppm[0]=0;
    }
    int cx=W/2, cy=(int)(H*0.55); unsigned char *c=&px[(cy*W+cx)*4], *tl=&px[(5*W+5)*4];
    int64_t packed = ((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    int corner_clear = tl[0]<60 && tl[1]<60 && tl[2]<60;
    return corner_clear ? packed : -1;
}

/* Headless render resolution — square, default 256, settable from Vire via vk_resolution(n).
 * The cache keys on it, so different sizes get their own pipeline. */
static uint32_t g_res=256;
/* --- coarse per-phase timing (behind FASTLLVM_VK_TIME) — measure the persistent-resource lever --- */
static double g_t_create=0, g_t_render=0, g_t_back=0; static long g_t_n=0; static int g_t_on=-1;
static double ts_ms(struct timespec a, struct timespec b){ return (b.tv_sec-a.tv_sec)*1e3 + (b.tv_nsec-a.tv_nsec)*1e-6; }
static void vk_time_report(void){ if(g_t_n) fprintf(stderr,"[vk-time] %ld frames: create=%.1fms render=%.1fms readback+destroy=%.1fms (per-frame create=%.3fms)\n", g_t_n, g_t_create, g_t_render, g_t_back, g_t_create/g_t_n); }
void jrt_vk_flush(void);   /* drain in-flight pipelined frames (defined after render_headless_d) */

static int64_t render_headless_d(const float *data, uint32_t nverts, uint32_t fpv, const float uni[4], const char *ppm, int depth) {
    uint32_t W=g_res, H=g_res;
    /* Use the ONE persistent context instead of creating a device per call — so an
     * animation (e.g. examples/vire/sphere.vr) pays the setup cost once, not per frame. */
    if(!ctx_init()) return 0;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;
    if(g_t_on<0){ g_t_on=getenv("FASTLLVM_VK_TIME")?1:0; if(g_t_on) atexit(vk_time_report);
        const char *pe=getenv("FASTLLVM_VK_PIPE"); if(pe){ int d=atoi(pe); if(d>=1&&d<=FRING_MAX) g_pipe_depth=d; }
        atexit(jrt_vk_flush); }
    struct timespec _t0,_t1,_t2,_t3; if(g_t_on) clock_gettime(CLOCK_MONOTONIC,&_t0);

    /* pipeline layout: fpv 2 → 2D, 5 → 2D+color, 6 → 3D+color (back-face culled). The
     * render pass + pipeline are cached (keyed by mode/depth/size), so the SPIR-V is
     * compiled once even across an animation's frames. */
    int vmode = (fpv==6)?2 : (fpv==5)?1 : 0;
    VkRenderPass rp; VkPipeline pipe; VkPipelineLayout pl;
    if(!rcache_get(dev,vmode,depth,W,H,&rp,&pipe,&pl)) return 0;
    /* All the per-frame images/buffers/pool/fence live in a ring (keyed by W,H,depth) — an
     * animation allocates them once and reuses them, so per frame we only upload the vertex
     * data, re-record, and submit. See fcache_get. */
    FCacheEntry *fc = fcache_get(dev,pd,qf,depth,W,H,rp,nverts*fpv);
    if(!fc) return 0;
    int R=fc->ring, idx=(int)(fc->seq % R);
    FSlot *sl=&fc->s[idx];
    /* Deferred pipeline (R>1): if this slot still holds an in-flight frame from R renders ago,
     * wait for it and harvest its result — THAT is what this call returns. Then record + submit
     * THIS frame without waiting, so up to R frames run on the GPU at once. R==1 collapses to
     * the classic path (harvest == this same frame → synchronous, returns this frame exactly). */
    int64_t deferred=0; int have_deferred=0;
    if(R>1 && sl->inflight){
        CK(vkWaitForFences(dev,1,&sl->fence,VK_TRUE,~0ull)); sl->inflight=0;
        deferred=fslot_harvest(W,H,sl); have_deferred=1;
    }
    memcpy(sl->vmap, data, (size_t)nverts*fpv*sizeof(float)); /* host-coherent: no flush */
    /* remember this frame's PPM target; written when the frame is later harvested */
    if(ppm){ size_t n=strlen(ppm); if(n>=sizeof(sl->ppm)) n=sizeof(sl->ppm)-1; memcpy(sl->ppm,ppm,n); sl->ppm[n]=0; }
    else sl->ppm[0]=0;
    vkResetCommandBuffer(sl->cmd,0);
    rec_draw_d(sl->cmd,rp,sl->fb,pipe,W,H,sl->vbuf,nverts,pl,uni,depth);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(sl->cmd,sl->img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,sl->sbuf,1,&rg);
    CK(vkEndCommandBuffer(sl->cmd));
    vkResetFences(dev,1,&sl->fence);
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&sl->cmd};
    if(g_t_on) clock_gettime(CLOCK_MONOTONIC,&_t1);
    CK(vkQueueSubmit(q,1,&si,sl->fence)); sl->inflight=1;
    fc->seq++;
    int64_t result;
    if(R>1){
        /* Don't block on THIS frame — the deferred (older) result is the return value. The last
         * R-1 frames stay in flight and are flushed at exit (fcache_flush). */
        result = have_deferred ? deferred : 0;
        if(g_t_on) clock_gettime(CLOCK_MONOTONIC,&_t2);
    } else {
        CK(vkWaitForFences(dev,1,&sl->fence,VK_TRUE,~0ull)); sl->inflight=0;
        if(g_t_on) clock_gettime(CLOCK_MONOTONIC,&_t2);
        result = fslot_harvest(W,H,sl);
    }
    /* Nothing is destroyed per frame: the ring lives in the fcache, the render pass/pipeline in
     * the rcache, the device/instance in the context. */
    if(g_t_on){ clock_gettime(CLOCK_MONOTONIC,&_t3); g_t_create+=ts_ms(_t0,_t1); g_t_render+=ts_ms(_t1,_t2); g_t_back+=ts_ms(_t2,_t3); g_t_n++; }
    return result;
}

/* Drain every still-in-flight ring frame — waits its fence and writes any pending PPM. Called
 * at exit so a pipelined (R>1) animation's trailing R-1 frames land on disk. */
void jrt_vk_flush(void){
    if(!g_ctx_ok) return;
    for(int i=0;i<8;i++){ FCacheEntry *e=&g_fcache[i]; if(!e->valid) continue;
        for(int k=0;k<e->ring;k++){ FSlot *sl=&e->s[k]; if(sl->inflight){
            vkWaitForFences(g_dev,1,&sl->fence,VK_TRUE,~0ull); sl->inflight=0; fslot_harvest(e->w,e->h,sl); } } }
}

/* Back-compat wrapper: no depth (2D or the back-face-culled 3D path). */
static int64_t render_headless(const float *data, uint32_t nverts, uint32_t fpv, const float uni[4], const char *ppm) {
    return render_headless_d(data, nverts, fpv, uni, ppm, 0);
}

/* The default triangle, from the compile-time corner buffer. */
int64_t jrt_vk_triangle(double a, double b, double c, double d) {
    float uni[4]={(float)a,(float)b,(float)c,(float)d};
    return render_headless(DEFAULT_TRI, 3, 2, uni, 0);
}

/* vk_frame_bg(r,g,b): the target of the declarative `frame { bg(r,g,b) }` — render a
 * frame cleared to (r,g,b) with no geometry, return the centroid (= the background). */
int64_t jrt_vk_frame_bg(double r, double g, double b) {
    enum { W=64, H=64 };
    /* persistent context — no per-call device (unified onto the one graphics device). */
    if(!ctx_init()) return -1;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;
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
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{(float)r,(float)g,(float)b,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE); vkCmdEndRenderPass(cmd);   /* clear only, no geometry */
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    unsigned char *c=&px[((H/2)*W+W/2)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyFramebuffer(dev,fb,0); vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    /* persistent device/instance — not destroyed. */
    return packed;
}

/* vk_textured(): render the triangle sampling a 2x2 RGBA texture (red, green, blue,
 * orange quadrants) — the @fragment reads it with tex(uv). Combined image sampler at
 * set 0 binding 0. Returns the centroid pixel packed 0xRRGGBB, or -1 on failure. */
int64_t jrt_vk_textured(void) {
    enum { W=256, H=256 };
    /* persistent context — no per-call device (unified onto the one graphics device). */
    if(!ctx_init()) return -1;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;

    /* color target + render pass + framebuffer */
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

    /* 2x2 texture: (0,0)=red (1,0)=green (0,1)=blue (1,1)=orange, LINEAR host-visible. */
    unsigned char texels[16]={255,0,0,255, 0,255,0,255, 0,0,255,255, 255,128,0,255};
    VkImageCreateInfo ti={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={2,2,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_LINEAR,
        .usage=VK_IMAGE_USAGE_SAMPLED_BIT,.initialLayout=VK_IMAGE_LAYOUT_PREINITIALIZED};
    VkImage tex; CK(vkCreateImage(dev,&ti,0,&tex));
    VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,tex,&tmr);
    uint32_t tt=find_mem(pd,tmr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(tt==~0u) return -1;
    VkMemoryAllocateInfo tma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=tmr.size,.memoryTypeIndex=tt};
    VkDeviceMemory tmem; CK(vkAllocateMemory(dev,&tma,0,&tmem)); vkBindImageMemory(dev,tex,tmem,0);
    VkImageSubresource sr={.aspectMask=VK_IMAGE_ASPECT_COLOR_BIT}; VkSubresourceLayout srl; vkGetImageSubresourceLayout(dev,tex,&sr,&srl);
    unsigned char *tp; CK(vkMapMemory(dev,tmem,0,tmr.size,0,(void**)&tp));
    for(int y=0;y<2;y++) memcpy(tp+srl.offset+y*srl.rowPitch, texels+y*8, 8);
    vkUnmapMemory(dev,tmem);
    VkImageViewCreateInfo tvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=tex,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView texview; CK(vkCreateImageView(dev,&tvi,0,&texview));
    VkSamplerCreateInfo smi={.sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,.magFilter=VK_FILTER_NEAREST,.minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE};
    VkSampler samp; CK(vkCreateSampler(dev,&smi,0,&samp));

    /* descriptor set — layout REFLECTED from the @fragment shader's tex() usage
     * (V3), not a hardcoded {binding 0, sampler, fragment}. The pool/set/write below
     * bind the host texture at binding 0, matching the reflected layout. */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=1};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    VkDescriptorImageInfo dii={.sampler=samp,.imageView=texview,.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet wds={.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&dii};
    vkUpdateDescriptorSets(dev,1,&wds,0,0);

    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl,0,dsl); if(!pipe) return -1;
    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,DEFAULT_TRI,6,&vbuf,&vmem)) return -1;

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
    /* transition the texture PREINITIALIZED → SHADER_READ_ONLY */
    VkImageMemoryBarrier tb={.sType=VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,.srcAccessMask=VK_ACCESS_HOST_WRITE_BIT,.dstAccessMask=VK_ACCESS_SHADER_READ_BIT,
        .oldLayout=VK_IMAGE_LAYOUT_PREINITIALIZED,.newLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,.image=tex,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_HOST_BIT,VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,0,0,0,0,0,1,&tb);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl,0,1,&dset,0,0);
    float zero[4]={0,0,0,0}; vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off);
    vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int cx=W/2, cy=(int)(H*0.55); unsigned char *c=&px[(cy*W+cx)*4];
    int64_t packed = ((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroySampler(dev,samp,0); vkDestroyImageView(dev,texview,0); vkDestroyImage(dev,tex,0); vkFreeMemory(dev,tmem,0);
    vkDestroyFramebuffer(dev,fb,0); vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    /* persistent device/instance — not destroyed. */
    return packed;
}

/* ---- Persistent context + RC-bound GPU texture handle -----------------------------
 * A lazily-created VkInstance/VkDevice shared across calls, so a GPU texture can OUTLIVE
 * a single draw. The handle is a Vire heap object (jrt_alloc) whose drop vtable slot
 * frees the GPU resources — so the texture's lifetime is tied to Vire's reference
 * counting: it is destroyed exactly when the last Vire reference drops, and a
 * use-after-free is impossible (you cannot name a dropped Vire value). This is the
 * lifetime-safe GPU resource handle. */
extern void *jrt_alloc(int64_t size);

/* The handle object: a Vire header (refcount, vtable) + the persistent GPU resources. */
typedef struct { int64_t refcount; void *vtable; VkImage image; VkDeviceMemory mem; VkImageView view; VkSampler sampler; uint32_t tw, th; } GpuTex;
static void gpu_tex_drop(void *p) {
    GpuTex *t=(GpuTex*)p;
    if(t->sampler) vkDestroySampler(g_dev,t->sampler,0);
    if(t->view) vkDestroyImageView(g_dev,t->view,0);
    if(t->image) vkDestroyImage(g_dev,t->image,0);
    if(t->mem) vkFreeMemory(g_dev,t->mem,0);
}
static void gpu_tex_trace(void *p, void (*visit)(void *)) { (void)p; (void)visit; } /* no ref fields */
static void *gpu_tex_vt[2] = { (void*)gpu_tex_drop, (void*)gpu_tex_trace };

/* A second RC-bound resource type: a persistent GPU storage buffer. Same lifetime-safe
 * model as the texture handle (a Vire object whose drop frees the GPU buffer), showing
 * the handle infrastructure generalizes beyond textures. */
typedef struct { int64_t refcount; void *vtable; VkBuffer buf; VkDeviceMemory mem; int64_t n; } GpuBuf;
static void gpu_buf_drop(void *p) {
    GpuBuf *b=(GpuBuf*)p;
    if(b->buf) vkDestroyBuffer(g_dev,b->buf,0);
    if(b->mem) vkFreeMemory(g_dev,b->mem,0);
}
static void gpu_buf_trace(void *p, void (*visit)(void *)) { (void)p; (void)visit; }
static void *gpu_buf_vt[2] = { (void*)gpu_buf_drop, (void*)gpu_buf_trace };

/* vk_buffer_new(data, n): upload `n` f64 values to a PERSISTENT GPU storage buffer and
 * return an RC-bound handle (freed when its last Vire reference drops). */
void *jrt_vk_buffer_new(const double *data, int64_t n) {
    if(!data || n<=0) return 0; if(!ctx_init()) return 0;
    VkDeviceSize sz=(VkDeviceSize)n*sizeof(float);
    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=sz,.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer buf; if(vkCreateBuffer(g_dev,&bi,0,&buf)!=VK_SUCCESS) return 0;
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(g_dev,buf,&mr);
    uint32_t mt=find_mem(g_pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(mt==~0u) return 0;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=mt};
    VkDeviceMemory mem; if(vkAllocateMemory(g_dev,&ma,0,&mem)!=VK_SUCCESS) return 0; vkBindBufferMemory(g_dev,buf,mem,0);
    float *p; if(vkMapMemory(g_dev,mem,0,sz,0,(void**)&p)!=VK_SUCCESS) return 0;
    for(int64_t i=0;i<n;i++) p[i]=(float)data[i];
    vkUnmapMemory(g_dev,mem);
    GpuBuf *h=(GpuBuf*)jrt_alloc((int64_t)sizeof(GpuBuf));
    h->vtable=gpu_buf_vt; h->buf=buf; h->mem=mem; h->n=n;
    return h;
}
/* vk_buffer_get(handle, i): read element i of the persistent GPU buffer (borrows). */
double jrt_vk_buffer_get(void *handle, int64_t i) {
    if(!handle || !g_ctx_ok) return 0.0;
    GpuBuf *b=(GpuBuf*)handle; if(i<0 || i>=b->n) return 0.0;
    float *p; if(vkMapMemory(g_dev,b->mem,0,VK_WHOLE_SIZE,0,(void**)&p)!=VK_SUCCESS) return 0.0;
    double v=(double)p[i]; vkUnmapMemory(g_dev,b->mem); return v;
}

/* A persistent headless RENDER SESSION (a third RC-bound resource): the render target,
 * pipeline, vertex buffer and readback buffer are created ONCE and reused across frames,
 * so a Vire-driven loop can render many frames without per-frame setup — the interactive
 * rendering core. vk_frame(session, r,g,b,a) renders one frame with the uniform and
 * returns the centroid; the session (and all its GPU objects) is freed when the RC
 * handle drops. */
enum { SW=256, SH=256 };
typedef struct { int64_t refcount; void *vtable;
    VkImage img; VkDeviceMemory imem; VkImageView view; VkRenderPass rp; VkFramebuffer fb;
    VkPipeline pipe; VkPipelineLayout pl; VkBuffer vbuf; VkDeviceMemory vmem; VkBuffer rbuf; VkDeviceMemory rbmem; VkCommandPool cp; } GpuSession;
static void gpu_session_drop(void *p) {
    GpuSession *s=(GpuSession*)p;
    if(s->cp) vkDestroyCommandPool(g_dev,s->cp,0);
    if(s->rbuf) vkDestroyBuffer(g_dev,s->rbuf,0); if(s->rbmem) vkFreeMemory(g_dev,s->rbmem,0);
    if(s->vbuf) vkDestroyBuffer(g_dev,s->vbuf,0); if(s->vmem) vkFreeMemory(g_dev,s->vmem,0);
    if(s->pipe) vkDestroyPipeline(g_dev,s->pipe,0); if(s->pl) vkDestroyPipelineLayout(g_dev,s->pl,0);
    if(s->fb) vkDestroyFramebuffer(g_dev,s->fb,0); if(s->rp) vkDestroyRenderPass(g_dev,s->rp,0);
    if(s->view) vkDestroyImageView(g_dev,s->view,0); if(s->img) vkDestroyImage(g_dev,s->img,0); if(s->imem) vkFreeMemory(g_dev,s->imem,0);
}
static void gpu_session_trace(void *p, void (*visit)(void *)) { (void)p; (void)visit; }
static void *gpu_session_vt[2] = { (void*)gpu_session_drop, (void*)gpu_session_trace };

/* vk_session(): build a persistent headless render session; return an RC-bound handle. */
void *jrt_vk_session(void) {
    if(!ctx_init()) return 0;
    GpuSession *s=(GpuSession*)jrt_alloc((int64_t)sizeof(GpuSession));
    memset((char*)s+sizeof(int64_t)+sizeof(void*), 0, sizeof(GpuSession)-sizeof(int64_t)-sizeof(void*));
    s->vtable=gpu_session_vt;
    VkImageCreateInfo ii={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={SW,SH,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    if(vkCreateImage(g_dev,&ii,0,&s->img)!=VK_SUCCESS) return s;
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(g_dev,s->img,&mr);
    uint32_t it=find_mem(g_pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return s;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    vkAllocateMemory(g_dev,&ma,0,&s->imem); vkBindImageMemory(g_dev,s->img,s->imem,0);
    VkImageViewCreateInfo ivi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=s->img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    vkCreateImageView(g_dev,&ivi,0,&s->view);
    s->rp=build_rp(g_dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=s->rp,.attachmentCount=1,.pAttachments=&s->view,.width=SW,.height=SH,.layers=1};
    vkCreateFramebuffer(g_dev,&fbi,0,&s->fb);
    s->pipe=build_pipeline(g_dev,s->rp,SW,SH,&s->pl,0,0);
    make_vbuf(g_dev,g_pd,DEFAULT_TRI,6,&s->vbuf,&s->vmem);
    VkBufferCreateInfo bi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=SW*SH*4,.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    vkCreateBuffer(g_dev,&bi,0,&s->rbuf);
    VkMemoryRequirements br; vkGetBufferMemoryRequirements(g_dev,s->rbuf,&br);
    uint32_t bt=find_mem(g_pd,br.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    VkMemoryAllocateInfo bm={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=br.size,.memoryTypeIndex=bt};
    vkAllocateMemory(g_dev,&bm,0,&s->rbmem); vkBindBufferMemory(g_dev,s->rbuf,s->rbmem,0);
    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,.queueFamilyIndex=g_gqf};
    vkCreateCommandPool(g_dev,&cpi,0,&s->cp);
    return s;
}
/* vk_frame(handle, r,g,b,a): render one frame with the given uniform (the @fragment
 * reads uniform()); return the centroid pixel 0xRRGGBB. Reuses the session's persistent
 * pipeline/target — no per-frame setup. Borrows the handle. */
int64_t jrt_vk_frame(void *handle, double r, double g, double b, double a) {
    if(!handle || !g_ctx_ok) return -1;
    GpuSession *s=(GpuSession*)handle; if(!s->pipe) return -1;
    float uni[4]={(float)r,(float)g,(float)b,(float)a};
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=s->cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; if(vkAllocateCommandBuffers(g_dev,&cai,&cmd)!=VK_SUCCESS) return -1;
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=s->rp,.framebuffer=s->fb,.renderArea={{0,0},{SW,SH}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,s->pipe);
    vkCmdPushConstants(cmd,s->pl,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,uni);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&s->vbuf,&off); vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={SW,SH,1}};
    vkCmdCopyImageToBuffer(cmd,s->img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,s->rbuf,1,&rg);
    vkEndCommandBuffer(cmd);
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fe; vkCreateFence(g_dev,&fci,0,&fe);
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    vkQueueSubmit(g_gq,1,&si,fe); vkWaitForFences(g_dev,1,&fe,VK_TRUE,~0ull);
    unsigned char *px; vkMapMemory(g_dev,s->rbmem,0,SW*SH*4,0,(void**)&px);
    int scx=SW/2, scy=(int)(SH*0.55); unsigned char *c=&px[(scy*SW+scx)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(g_dev,s->rbmem);
    vkDestroyFence(g_dev,fe,0); vkFreeCommandBuffers(g_dev,s->cp,1,&cmd);
    return packed;
}

/* vk_texture_new(pixels, nfloats, w): create a PERSISTENT GPU texture from Vire data and
 * return an RC-bound handle (a Vire object). The GPU texture lives until the handle's
 * last reference drops (Vire RC → gpu_tex_drop). Returns 0 on failure. */
void *jrt_vk_texture_new(const double *pixels, int64_t nfloats, int64_t w) {
    if(!pixels || w<=0 || nfloats < 4*w || (nfloats % (4*w))!=0) return 0;
    if(!ctx_init()) return 0;
    uint32_t TW=(uint32_t)w, TH=(uint32_t)(nfloats/(4*w));
    VkImageCreateInfo ti={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={TW,TH,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_LINEAR,
        .usage=VK_IMAGE_USAGE_SAMPLED_BIT,.initialLayout=VK_IMAGE_LAYOUT_PREINITIALIZED};
    VkImage tex; if(vkCreateImage(g_dev,&ti,0,&tex)!=VK_SUCCESS) return 0;
    VkMemoryRequirements tmr; vkGetImageMemoryRequirements(g_dev,tex,&tmr);
    uint32_t tt=find_mem(g_pd,tmr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(tt==~0u) return 0;
    VkMemoryAllocateInfo tma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=tmr.size,.memoryTypeIndex=tt};
    VkDeviceMemory tmem; if(vkAllocateMemory(g_dev,&tma,0,&tmem)!=VK_SUCCESS) return 0; vkBindImageMemory(g_dev,tex,tmem,0);
    VkImageSubresource sr={.aspectMask=VK_IMAGE_ASPECT_COLOR_BIT}; VkSubresourceLayout srl; vkGetImageSubresourceLayout(g_dev,tex,&sr,&srl);
    unsigned char *tp; if(vkMapMemory(g_dev,tmem,0,tmr.size,0,(void**)&tp)!=VK_SUCCESS) return 0;
    for(uint32_t y=0;y<TH;y++) for(uint32_t x=0;x<TW;x++){
        const double *s=&pixels[(y*TW+x)*4]; unsigned char *o=tp+srl.offset+y*srl.rowPitch+x*4;
        for(int k=0;k<4;k++){ double v=s[k]; if(v<0)v=0; if(v>1)v=1; o[k]=(unsigned char)(v*255.0+0.5); }
    }
    vkUnmapMemory(g_dev,tmem);
    VkImageViewCreateInfo tvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=tex,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView texview; if(vkCreateImageView(g_dev,&tvi,0,&texview)!=VK_SUCCESS) return 0;
    VkSamplerCreateInfo smi={.sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,.magFilter=VK_FILTER_NEAREST,.minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE};
    VkSampler samp; if(vkCreateSampler(g_dev,&smi,0,&samp)!=VK_SUCCESS) return 0;
    /* transition the texture to SHADER_READ_ONLY once, on a throwaway command buffer */
    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=g_gqf};
    VkCommandPool cp; vkCreateCommandPool(g_dev,&cpi,0,&cp);
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; vkAllocateCommandBuffers(g_dev,&cai,&cmd);
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    auto_barrier(cmd,tex,VK_IMAGE_LAYOUT_PREINITIALIZED,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fe; vkCreateFence(g_dev,&fci,0,&fe);
    vkQueueSubmit(g_gq,1,&si,fe); vkWaitForFences(g_dev,1,&fe,VK_TRUE,~0ull);
    vkDestroyFence(g_dev,fe,0); vkDestroyCommandPool(g_dev,cp,0);

    GpuTex *h=(GpuTex*)jrt_alloc((int64_t)sizeof(GpuTex));
    h->vtable=gpu_tex_vt; h->image=tex; h->mem=tmem; h->view=texview; h->sampler=samp; h->tw=TW; h->th=TH;
    return h;
}

/* vk_draw_handle(handle): render the triangle sampling the persistent texture the handle
 * owns (no re-upload). Borrows the handle (does not release it). Returns the centroid
 * pixel 0xRRGGBB, or -1. */
/* Draw program geometry (`fdata` = `nverts` interleaved (x,y) f32) sampling the texture
 * `handle`, with the vec4 `uni` pushed. The descriptor-set layout is REFLECTED from the
 * shader (mk_dsl_reflected), and the handle's texture is written to the reflected sampler
 * binding — so a single generic path binds the reflected resource. Persistent context. */
static int64_t draw_res_geo(void **handles, uint32_t nh, const float *fdata, uint32_t nverts, const float uni[4]) {
    if((nh && !handles) || !g_ctx_ok) return -1;
    enum { W=256, H=256 };
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;
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
    /* descriptor-set layout reflected from the @fragment's tex() usage (V3) */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
    /* Bind each supplied handle to ITS reflected binding — the caller passes handles in
     * binding order, and each goes to VK_IFACE_BINDING[i], so multiple reflected samplers
     * (e.g. a 2-texture blend @fragment) are bound generically from the shader interface. */
    unsigned nb = VK_IFACE_NB < nh ? VK_IFACE_NB : nh; if(nb>8) nb=8;
    /* Pool sized per descriptor KIND (reflected): samplers + storage buffers. */
    unsigned nsamp=0, nbuf=0;
    for(unsigned i=0;i<nb;i++){ if(VK_IFACE_KIND[i]==1) nsamp++; else nbuf++; }
    VkDescriptorPoolSize dpsz[2]; unsigned nps=0;
    if(nsamp) dpsz[nps++]=(VkDescriptorPoolSize){.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=nsamp};
    if(nbuf)  dpsz[nps++]=(VkDescriptorPoolSize){.type=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.descriptorCount=nbuf};
    if(!nps){ dpsz[0]=(VkDescriptorPoolSize){.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=1}; nps=1; }
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=nps,.pPoolSizes=dpsz};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    /* Write each handle per its reflected KIND — a GpuTex to a sampler binding, a
     * GpuBuf to a storage-buffer binding — so one path binds textures AND buffers. */
    VkDescriptorImageInfo dii[8]; VkDescriptorBufferInfo dbi[8]; VkWriteDescriptorSet wds[8];
    for(unsigned i=0;i<nb;i++){
        if(VK_IFACE_KIND[i]==1){
            GpuTex *ht=(GpuTex*)handles[i];
            dii[i]=(VkDescriptorImageInfo){.sampler=ht->sampler,.imageView=ht->view,.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
            wds[i]=(VkWriteDescriptorSet){.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=VK_IFACE_BINDING[i],.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&dii[i]};
        } else {
            GpuBuf *hb=(GpuBuf*)handles[i];
            dbi[i]=(VkDescriptorBufferInfo){.buffer=hb->buf,.offset=0,.range=VK_WHOLE_SIZE};
            wds[i]=(VkWriteDescriptorSet){.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=VK_IFACE_BINDING[i],.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,.pBufferInfo=&dbi[i]};
        }
    }
    if(nb) vkUpdateDescriptorSets(dev,nb,wds,0,0);
    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl,0,dsl); if(!pipe) return -1;
    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,fdata,nverts*2,&vbuf,&vmem)) return -1;
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
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl,0,1,&dset,0,0);
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,uni);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,nverts,1,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int scx=W/2, scy=(int)(H*0.55); unsigned char *c=&px[(scy*W+scx)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroyFramebuffer(dev,fb,0); vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    return packed;   /* handle (persistent texture) NOT destroyed — lives until its RC drops */
}

/* vk_draw_handle(h): draw the built-in triangle sampling the texture handle `h`. */
int64_t jrt_vk_draw_handle(void *handle) {
    float zero[4]={0,0,0,0}; void *hs[1]={handle};
    return draw_res_geo(hs, 1, DEFAULT_TRI, 3, zero);
}

/* vk_draw_tex(verts, h, ux,uy,uz,uw): the generic draw surface WITH a reflected resource
 * — the program supplies its own geometry AND a texture handle AND a vec4 uniform. The
 * texture is bound to the sampler binding the @fragment's tex() reflects into, the
 * uniform reaches uniform(), and the geometry is the program's. So one generic draw
 * covers the textured case: pipeline + descriptor layout from the shader, resource +
 * geometry + parameters from the program. Returns the centroid pixel 0xRRGGBB. */
int64_t jrt_vk_draw_tex(const double *verts, int64_t nfloats, void *handle,
                        double ux, double uy, double uz, double uw) {
    if(!verts || nfloats < 6 || (nfloats % 2)!=0) return -1;
    uint32_t nverts=(uint32_t)(nfloats/2);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    float uni[4]={(float)ux,(float)uy,(float)uz,(float)uw}; void *hs[1]={handle};
    int64_t r=draw_res_geo(hs, 1, f, nverts, uni);
    free(f);
    return r;
}

/* vk_draw_buf(verts, h, ux,uy,uz,uw): the generic draw with a reflected STORAGE BUFFER —
 * the GpuBuf handle `h` binds to the storage-buffer binding the @fragment's buf(i)
 * reflects into (kind switch in draw_res_geo), so a data-driven fragment reads the Vire
 * GPU buffer. Program geometry + uniform as usual. Returns 0xRRGGBB. */
int64_t jrt_vk_draw_buf(const double *verts, int64_t nfloats, void *handle,
                        double ux, double uy, double uz, double uw) {
    if(!verts || nfloats < 6 || (nfloats % 2)!=0) return -1;
    uint32_t nverts=(uint32_t)(nfloats/2);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    float uni[4]={(float)ux,(float)uy,(float)uz,(float)uw}; void *hs[1]={handle};
    int64_t r=draw_res_geo(hs, 1, f, nverts, uni);
    free(f);
    return r;
}

/* vk_draw_tex_buf(verts, tex_h, buf_h, ux,uy,uz,uw): the generic draw with MIXED reflected
 * resources — a texture (sampler binding 0) AND a storage buffer (binding 2) in one draw,
 * for a @fragment that uses both tex() and buf(). Exercises the per-binding kind switch
 * with heterogeneous descriptor types. draw_res_geo binds by the reflected order (the
 * SSBO comes first), so the handles are passed {buf, tex}. Returns 0xRRGGBB. */
int64_t jrt_vk_draw_tex_buf(const double *verts, int64_t nfloats, void *tex_h, void *buf_h,
                            double ux, double uy, double uz, double uw) {
    if(!verts || nfloats < 6 || (nfloats % 2)!=0) return -1;
    uint32_t nverts=(uint32_t)(nfloats/2);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    float uni[4]={(float)ux,(float)uy,(float)uz,(float)uw}; void *hs[2]={buf_h,tex_h};
    int64_t r=draw_res_geo(hs, 2, f, nverts, uni);
    free(f);
    return r;
}

/* vk_draw_tex2(verts, h0, h1, ux,uy,uz,uw): the generic draw with TWO reflected samplers
 * — h0 binds to the shader's first reflected sampler binding, h1 to the second (e.g. a
 * two-texture blend @fragment reading tex() and tex2()). Multiple resources, all bound
 * from the shader interface, plus program geometry + uniform. Returns 0xRRGGBB. */
int64_t jrt_vk_draw_tex2(const double *verts, int64_t nfloats, void *h0, void *h1,
                         double ux, double uy, double uz, double uw) {
    if(!verts || nfloats < 6 || (nfloats % 2)!=0) return -1;
    uint32_t nverts=(uint32_t)(nfloats/2);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    float uni[4]={(float)ux,(float)uy,(float)uz,(float)uw}; void *hs[2]={h0,h1};
    int64_t r=draw_res_geo(hs, 2, f, nverts, uni);
    free(f);
    return r;
}

/* vk_texture_draw(pixels, nfloats, w): a texture as a first-class Vire value. `pixels`
 * is a Vire [Float] of interleaved RGBA in 0..1 (4 per texel), width `w`, height
 * nfloats/(4w) — an RC-managed Vire array, so the "texture handle" is lifetime-safe by
 * construction (no GPU resource outlives the call; the value can be stored/reused/passed
 * with no use-after-free). Uploads it to a GPU texture, renders the triangle sampling it
 * (the program's tex(uv) @fragment), returns the centroid pixel 0xRRGGBB, or -1. */
int64_t jrt_vk_texture_draw(const double *pixels, int64_t nfloats, int64_t w) {
    if(!pixels || w<=0 || nfloats < 4*w || (nfloats % (4*w))!=0) return -1;
    uint32_t TW=(uint32_t)w, TH=(uint32_t)(nfloats/(4*w));
    enum { W=256, H=256 };
    /* persistent context — no per-call device (unified onto the one graphics device). */
    if(!ctx_init()) return -1;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;

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

    /* the Vire-supplied texture (TW x TH), LINEAR host-visible, converted f64→u8 */
    VkImageCreateInfo ti={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={TW,TH,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_LINEAR,
        .usage=VK_IMAGE_USAGE_SAMPLED_BIT,.initialLayout=VK_IMAGE_LAYOUT_PREINITIALIZED};
    VkImage tex; CK(vkCreateImage(dev,&ti,0,&tex));
    VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,tex,&tmr);
    uint32_t tt=find_mem(pd,tmr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(tt==~0u) return -1;
    VkMemoryAllocateInfo tma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=tmr.size,.memoryTypeIndex=tt};
    VkDeviceMemory tmem; CK(vkAllocateMemory(dev,&tma,0,&tmem)); vkBindImageMemory(dev,tex,tmem,0);
    VkImageSubresource sr={.aspectMask=VK_IMAGE_ASPECT_COLOR_BIT}; VkSubresourceLayout srl; vkGetImageSubresourceLayout(dev,tex,&sr,&srl);
    unsigned char *tp; CK(vkMapMemory(dev,tmem,0,tmr.size,0,(void**)&tp));
    for(uint32_t y=0;y<TH;y++) for(uint32_t x=0;x<TW;x++){
        const double *s=&pixels[(y*TW+x)*4]; unsigned char *o=tp+srl.offset+y*srl.rowPitch+x*4;
        for(int k=0;k<4;k++){ double v=s[k]; if(v<0)v=0; if(v>1)v=1; o[k]=(unsigned char)(v*255.0+0.5); }
    }
    vkUnmapMemory(dev,tmem);
    VkImageViewCreateInfo tvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=tex,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView texview; CK(vkCreateImageView(dev,&tvi,0,&texview));
    VkSamplerCreateInfo smi={.sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,.magFilter=VK_FILTER_NEAREST,.minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE};
    VkSampler samp; CK(vkCreateSampler(dev,&smi,0,&samp));
    /* descriptor-set layout reflected from the @fragment's tex() usage (V3) */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=1};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    VkDescriptorImageInfo dii={.sampler=samp,.imageView=texview,.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet wds={.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&dii};
    vkUpdateDescriptorSets(dev,1,&wds,0,0);

    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl,0,dsl); if(!pipe) return -1;
    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,DEFAULT_TRI,6,&vbuf,&vmem)) return -1;
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
    auto_barrier(cmd,tex,VK_IMAGE_LAYOUT_PREINITIALIZED,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl,0,1,&dset,0,0);
    float zero[4]={0,0,0,0}; vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int scx=W/2, scy=(int)(H*0.55); unsigned char *c=&px[(scy*W+scx)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroySampler(dev,samp,0); vkDestroyImageView(dev,texview,0); vkDestroyImage(dev,tex,0); vkFreeMemory(dev,tmem,0);
    vkDestroyFramebuffer(dev,fb,0); vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    /* persistent device/instance — not destroyed. */
    return packed;
}

/* Helper: create an offscreen colour texture (attachment+sampled+transfer_src) + view +
 * framebuffer against `rp`. Returns 1 on success. */
static int mk_target(VkDevice dev, VkPhysicalDevice pd, uint32_t w, uint32_t h, VkRenderPass rp,
                     VkImage *img, VkDeviceMemory *mem, VkImageView *view, VkFramebuffer *fb) {
    VkImageCreateInfo ci={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={w,h,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_SAMPLED_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    if(vkCreateImage(dev,&ci,0,img)!=VK_SUCCESS) return 0;
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,*img,&mr);
    uint32_t mt=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(mt==~0u) return 0;
    VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=mt};
    if(vkAllocateMemory(dev,&ma,0,mem)!=VK_SUCCESS) return 0; vkBindImageMemory(dev,*img,*mem,0);
    VkImageViewCreateInfo vi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=*img,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    if(vkCreateImageView(dev,&vi,0,view)!=VK_SUCCESS) return 0;
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=view,.width=w,.height=h,.layers=1};
    return vkCreateFramebuffer(dev,&fbi,0,fb)==VK_SUCCESS;
}

/* vk_blend2(): a render graph with a MULTI-INPUT pass (a DAG, not a chain). Two source
 * passes render red → A and blue → B; a third pass samples BOTH A and B (the program's
 * tex(uv)+tex2(uv) @fragment) and outputs. The runtime auto-transitions BOTH inputs to
 * SHADER_READ_ONLY before the blend pass — dependency-driven barriers for a fan-in.
 * Returns the centroid pixel 0xRRGGBB. */
int64_t jrt_vk_blend2(void) {
    enum { W=256, H=256 };
    /* persistent context — no per-call device (unified onto the one graphics device). */
    if(!ctx_init()) return -1;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;

    VkRenderPass rp=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL); if(!rp) return -1;
    VkImage A,B,Cc; VkDeviceMemory Am,Bm,Cm; VkImageView Av,Bv,Cv; VkFramebuffer Af,Bf,Cf;
    if(!mk_target(dev,pd,W,H,rp,&A,&Am,&Av,&Af)) return -1;
    if(!mk_target(dev,pd,W,H,rp,&B,&Bm,&Bv,&Bf)) return -1;
    if(!mk_target(dev,pd,W,H,rp,&Cc,&Cm,&Cv,&Cf)) return -1;

    VkPipelineLayout plR; VkPipeline pipeR=build_pipeline_f(dev,rp,W,H,&plR,0,0,VK_PASS1_FRAG,VK_PASS1_FRAG_N); if(!pipeR) return -1;
    VkPipelineLayout plB; VkPipeline pipeB=build_pipeline_f(dev,rp,W,H,&plB,0,0,VK_PASS2_FRAG,VK_PASS2_FRAG_N); if(!pipeB) return -1;
    /* blend pipeline: 2 combined image samplers (binding 0 = A, binding 1 = B),
     * reflected from the blend @fragment's tex()/tex2() usage (V3). */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
    VkPipelineLayout plC; VkPipeline pipeC=build_pipeline(dev,rp,W,H,&plC,0,dsl); if(!pipeC) return -1;
    VkSamplerCreateInfo smi={.sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,.magFilter=VK_FILTER_NEAREST,.minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE};
    VkSampler samp; CK(vkCreateSampler(dev,&smi,0,&samp));
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=2};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    VkDescriptorImageInfo iiA={.sampler=samp,.imageView=Av,.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkDescriptorImageInfo iiB={.sampler=samp,.imageView=Bv,.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet w2[2]={
        {.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&iiA},
        {.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=1,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&iiB}};
    vkUpdateDescriptorSets(dev,2,w2,0,0);

    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,DEFAULT_TRI,6,&vbuf,&vmem)) return -1;
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
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}}; VkDeviceSize off=0; float zero[4]={0,0,0,0};
    /* source pass A (red) */
    VkRenderPassBeginInfo ra={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=Af,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&ra,VK_SUBPASS_CONTENTS_INLINE); vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipeR);
    vkCmdPushConstants(cmd,plR,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0); vkCmdEndRenderPass(cmd);
    /* source pass B (blue) */
    VkRenderPassBeginInfo rb={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=Bf,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rb,VK_SUBPASS_CONTENTS_INLINE); vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipeB);
    vkCmdPushConstants(cmd,plB,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0); vkCmdEndRenderPass(cmd);
    /* fan-in: both inputs auto-transitioned before the blend pass */
    auto_barrier(cmd,A,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    auto_barrier(cmd,B,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    /* blend pass C (samples A + B) */
    VkRenderPassBeginInfo rc={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=Cf,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rc,VK_SUBPASS_CONTENTS_INLINE); vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipeC);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,plC,0,1,&dset,0,0);
    vkCmdPushConstants(cmd,plC,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0); vkCmdEndRenderPass(cmd);
    auto_barrier(cmd,Cc,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,Cc,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int scx=W/2, scy=(int)(H*0.55); unsigned char *c=&px[(scy*W+scx)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroySampler(dev,samp,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroyPipeline(dev,pipeC,0); vkDestroyPipelineLayout(dev,plC,0); vkDestroyPipeline(dev,pipeB,0); vkDestroyPipelineLayout(dev,plB,0); vkDestroyPipeline(dev,pipeR,0); vkDestroyPipelineLayout(dev,plR,0);
    vkDestroyFramebuffer(dev,Cf,0); vkDestroyImageView(dev,Cv,0); vkDestroyImage(dev,Cc,0); vkFreeMemory(dev,Cm,0);
    vkDestroyFramebuffer(dev,Bf,0); vkDestroyImageView(dev,Bv,0); vkDestroyImage(dev,B,0); vkFreeMemory(dev,Bm,0);
    vkDestroyFramebuffer(dev,Af,0); vkDestroyImageView(dev,Av,0); vkDestroyImage(dev,A,0); vkFreeMemory(dev,Am,0);
    vkDestroyRenderPass(dev,rp,0); /* persistent device/instance not destroyed. */
    return packed;
}

/* vk_chain(n): an N-pass render graph. Pass 0 renders red into texture T[0]; each pass
 * i (1..n) samples T[i-1] into T[i]; a final copy reads T[n] back. The runtime TRACKS
 * each texture's layout and auto-inserts the barrier for every hop (auto_barrier derives
 * it) — the render graph deepened from a fixed 2 passes to an arbitrary chain with
 * automatic, dependency-driven layout transitions. Returns the centroid pixel 0xRRGGBB. */
int64_t jrt_vk_chain(int64_t n) {
    if(n<1) n=1; if(n>7) n=7;
    uint32_t NT=(uint32_t)n+1;                  /* T[0..n] */
    enum { W=256, H=256 };
    /* persistent context — no per-call device (unified onto the one graphics device). */
    if(!ctx_init()) return -1;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;

    /* N+1 textures (color attachment + sampled), and per-texture tracked layout. */
    VkImage Ti[8]; VkDeviceMemory Tm[8]; VkImageView Tv[8]; VkFramebuffer Tf[8]; VkImageLayout Tl[8];
    VkRenderPass rp=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL); if(!rp) return -1;
    for(uint32_t i=0;i<NT;i++){
        VkImageCreateInfo ci={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
            .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
            .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_SAMPLED_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
        CK(vkCreateImage(dev,&ci,0,&Ti[i]));
        VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,Ti[i],&mr);
        uint32_t mt=find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(mt==~0u) return -1;
        VkMemoryAllocateInfo ma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=mt};
        CK(vkAllocateMemory(dev,&ma,0,&Tm[i])); vkBindImageMemory(dev,Ti[i],Tm[i],0);
        VkImageViewCreateInfo vi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=Ti[i],.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
        CK(vkCreateImageView(dev,&vi,0,&Tv[i]));
        VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&Tv[i],.width=W,.height=H,.layers=1};
        CK(vkCreateFramebuffer(dev,&fbi,0,&Tf[i]));
        Tl[i]=VK_IMAGE_LAYOUT_UNDEFINED;
    }
    /* red pipeline (pass 0) and sampling pipeline (passes 1..n) */
    VkPipelineLayout pl0; VkPipeline pipeRed=build_pipeline_f(dev,rp,W,H,&pl0,0,0,VK_PASS1_FRAG,VK_PASS1_FRAG_N); if(!pipeRed) return -1;
    /* descriptor-set layout reflected from the @fragment's tex() usage (V3) */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
    VkPipelineLayout plS; VkPipeline pipeSamp=build_pipeline(dev,rp,W,H,&plS,0,dsl); if(!pipeSamp) return -1;
    VkSamplerCreateInfo smi={.sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,.magFilter=VK_FILTER_NEAREST,.minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE};
    VkSampler samp; CK(vkCreateSampler(dev,&smi,0,&samp));
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=NT};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=NT,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));

    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,DEFAULT_TRI,6,&vbuf,&vmem)) return -1;
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
    VkCommandBufferBeginInfo cbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkDeviceSize off=0; float zero[4]={0,0,0,0};
    VkDescriptorSet sets[8]={0};
    for(uint32_t pass=0; pass<NT; pass++){
        if(pass>0){
            /* the graph auto-transitions the input (T[pass-1]) from its tracked layout */
            auto_barrier(cmd,Ti[pass-1],Tl[pass-1],VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL); Tl[pass-1]=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
            VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
            vkAllocateDescriptorSets(dev,&dsai,&sets[pass]);
            VkDescriptorImageInfo dii={.sampler=samp,.imageView=Tv[pass-1],.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
            VkWriteDescriptorSet wds={.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=sets[pass],.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&dii};
            vkUpdateDescriptorSets(dev,1,&wds,0,0);
        }
        VkRenderPassBeginInfo r={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=Tf[pass],.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
        vkCmdBeginRenderPass(cmd,&r,VK_SUBPASS_CONTENTS_INLINE);
        if(pass==0){ vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipeRed); vkCmdPushConstants(cmd,pl0,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero); }
        else { vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipeSamp); vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,plS,0,1,&sets[pass],0,0); vkCmdPushConstants(cmd,plS,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero); }
        vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0);
        vkCmdEndRenderPass(cmd);
        Tl[pass]=VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    }
    /* final: transition T[n] to transfer-src (auto) and read it back */
    auto_barrier(cmd,Ti[NT-1],Tl[NT-1],VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,Ti[NT-1],VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int scx=W/2, scy=(int)(H*0.55); unsigned char *c=&px[(scy*W+scx)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroySampler(dev,samp,0);
    vkDestroyPipeline(dev,pipeSamp,0); vkDestroyPipelineLayout(dev,plS,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroyPipeline(dev,pipeRed,0); vkDestroyPipelineLayout(dev,pl0,0);
    for(uint32_t i=0;i<NT;i++){ vkDestroyFramebuffer(dev,Tf[i],0); vkDestroyImageView(dev,Tv[i],0); vkDestroyImage(dev,Ti[i],0); vkFreeMemory(dev,Tm[i],0); }
    vkDestroyRenderPass(dev,rp,0);
    /* persistent device/instance — not destroyed. */
    return packed;
}

/* vk_two_pass(): a minimal render graph. Pass 1 renders the triangle (fixed red) to an
 * offscreen texture; the runtime auto-transitions it COLOR_ATTACHMENT → SHADER_READ_ONLY
 * (auto_barrier derives the barrier from the layouts); pass 2 renders sampling that
 * texture with the program's @fragment (tex(uv)). Returns the centroid pixel 0xRRGGBB.
 * Demonstrates automatic layout transitions + resource lifetimes between passes. */
int64_t jrt_vk_two_pass(void) {
    enum { W=256, H=256 };
    /* persistent context — no per-call device (unified onto the one graphics device). */
    if(!ctx_init()) return -1;
    VkDevice dev=g_dev; VkPhysicalDevice pd=g_pd; VkQueue q=g_gq; uint32_t qf=g_gqf;

    /* the intermediate texture T (pass-1 target, pass-2 source) */
    VkImageCreateInfo ti={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_SAMPLED_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage T; CK(vkCreateImage(dev,&ti,0,&T));
    VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,T,&tmr);
    uint32_t tt=find_mem(pd,tmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(tt==~0u) return -1;
    VkMemoryAllocateInfo tma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=tmr.size,.memoryTypeIndex=tt};
    VkDeviceMemory tmem; CK(vkAllocateMemory(dev,&tma,0,&tmem)); vkBindImageMemory(dev,T,tmem,0);
    VkImageViewCreateInfo tvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=T,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView Tview; CK(vkCreateImageView(dev,&tvi,0,&Tview));
    /* final color image C (pass-2 target, read back) */
    VkImageCreateInfo ci2={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage C; CK(vkCreateImage(dev,&ci2,0,&C));
    VkMemoryRequirements cmr; vkGetImageMemoryRequirements(dev,C,&cmr);
    uint32_t ct=find_mem(pd,cmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(ct==~0u) return -1;
    VkMemoryAllocateInfo cma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=cmr.size,.memoryTypeIndex=ct};
    VkDeviceMemory cmem; CK(vkAllocateMemory(dev,&cma,0,&cmem)); vkBindImageMemory(dev,C,cmem,0);
    VkImageViewCreateInfo cvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=C,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView Cview; CK(vkCreateImageView(dev,&cvi,0,&Cview));

    /* pass-1 render pass (leaves T in COLOR_ATTACHMENT_OPTIMAL for the auto barrier) */
    VkRenderPass rp1=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL); if(!rp1) return -1;
    VkFramebufferCreateInfo fb1i={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp1,.attachmentCount=1,.pAttachments=&Tview,.width=W,.height=H,.layers=1};
    VkFramebuffer fb1; CK(vkCreateFramebuffer(dev,&fb1i,0,&fb1));
    VkPipelineLayout pl1; VkPipeline pipe1=build_pipeline_f(dev,rp1,W,H,&pl1,0,0,VK_PASS1_FRAG,VK_PASS1_FRAG_N); if(!pipe1) return -1;

    /* pass-2 render pass (samples T) + texture descriptor */
    VkRenderPass rp2=build_rp(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL); if(!rp2) return -1;
    VkFramebufferCreateInfo fb2i={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp2,.attachmentCount=1,.pAttachments=&Cview,.width=W,.height=H,.layers=1};
    VkFramebuffer fb2; CK(vkCreateFramebuffer(dev,&fb2i,0,&fb2));
    VkSamplerCreateInfo smi={.sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,.magFilter=VK_FILTER_NEAREST,.minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE};
    VkSampler samp; CK(vkCreateSampler(dev,&smi,0,&samp));
    /* descriptor-set layout reflected from the @fragment's tex() usage (V3) */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
    VkDescriptorPoolSize dps={.type=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.descriptorCount=1};
    VkDescriptorPoolCreateInfo dpci={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,.maxSets=1,.poolSizeCount=1,.pPoolSizes=&dps};
    VkDescriptorPool dpool; CK(vkCreateDescriptorPool(dev,&dpci,0,&dpool));
    VkDescriptorSetAllocateInfo dsai={.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,.descriptorPool=dpool,.descriptorSetCount=1,.pSetLayouts=&dsl};
    VkDescriptorSet dset; CK(vkAllocateDescriptorSets(dev,&dsai,&dset));
    VkDescriptorImageInfo dii={.sampler=samp,.imageView=Tview,.imageLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet wds={.sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,.dstSet=dset,.dstBinding=0,.descriptorCount=1,.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,.pImageInfo=&dii};
    vkUpdateDescriptorSets(dev,1,&wds,0,0);
    VkPipelineLayout pl2; VkPipeline pipe2=build_pipeline(dev,rp2,W,H,&pl2,0,dsl); if(!pipe2) return -1;

    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,DEFAULT_TRI,6,&vbuf,&vmem)) return -1;
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
    VkDeviceSize off=0; float zero[4]={0,0,0,0};
    /* pass 1: draw fixed-red triangle into T */
    VkRenderPassBeginInfo r1={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp1,.framebuffer=fb1,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&r1,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe1);
    vkCmdPushConstants(cmd,pl1,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
    /* automatic layout transition: T is now a shader-readable texture */
    auto_barrier(cmd,T,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    /* pass 2: draw into C sampling T */
    VkRenderPassBeginInfo r2={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp2,.framebuffer=fb2,.renderArea={{0,0},{W,H}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&r2,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe2);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl2,0,1,&dset,0,0);
    vkCmdPushConstants(cmd,pl2,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,16,zero);
    vkCmdBindVertexBuffers(cmd,0,1,&vbuf,&off); vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,C,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int scx=W/2, scy=(int)(H*0.55); unsigned char *c=&px[(scy*W+scx)*4];
    int64_t packed=((int64_t)c[0]<<16)|((int64_t)c[1]<<8)|(int64_t)c[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,vbuf,0); vkFreeMemory(dev,vmem,0);
    vkDestroyPipeline(dev,pipe2,0); vkDestroyPipelineLayout(dev,pl2,0); vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0); vkDestroySampler(dev,samp,0);
    vkDestroyPipeline(dev,pipe1,0); vkDestroyPipelineLayout(dev,pl1,0);
    vkDestroyFramebuffer(dev,fb2,0); vkDestroyRenderPass(dev,rp2,0); vkDestroyFramebuffer(dev,fb1,0); vkDestroyRenderPass(dev,rp1,0);
    vkDestroyImageView(dev,Cview,0); vkDestroyImage(dev,C,0); vkFreeMemory(dev,cmem,0);
    vkDestroyImageView(dev,Tview,0); vkDestroyImage(dev,T,0); vkFreeMemory(dev,tmem,0);
    /* persistent device/instance — not destroyed. */
    return packed;
}

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
    VkInstance inst; CK(VK_CREATE_INSTANCE(&ici,&inst));
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
    VkPipelineLayout pl; VkPipeline pipe=build_mesh_pipeline(dev,rp,W,H,&pl,0,0); if(!pipe) return -1;
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
    float uni[4]={0,0,0,0};
    int64_t r=render_headless(f, nverts, fpv, uni, 0);
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

/* vk_render_ppm(verts, idx): render per-vertex-COLORED geometry (interleaved x,y, r,g,b —
 * 5 per vertex, as vk_mesh_c) to the image file `frame_NNN.ppm` (idx = NNN). The @vertex
 * forwards attr_color() via out_color(); the @fragment reads in_color(). Lets a Vire
 * program render an animation frame-by-frame to viewable files (e.g. a rotating sphere:
 * make a GIF with `convert frame_*.ppm out.gif`). Returns 0 on success, -1 on failure. */
int64_t jrt_vk_render_ppm(const double *verts, int64_t nfloats, int64_t idx) {
    if(!verts || nfloats < 15 || (nfloats % 5)!=0) return -1;   /* >=3 (x,y,r,g,b) vertices */
    uint32_t nverts=(uint32_t)(nfloats/5);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    char path[64]; snprintf(path,sizeof path,"frame_%03d.ppm",(int)idx);
    float uni[4]={0,0,0,0};
    int64_t r=render_headless(f, nverts, 5, uni, path);
    free(f);
    return r>=0 ? 0 : -1;
}

/* vk_resolution(n): set the headless render resolution to n x n pixels (16..4096).
 * The next render uses it; each size gets its own cached pipeline. Returns n. */
int64_t jrt_vk_set_resolution(int64_t n) {
    if(n < 16) n = 16; if(n > 4096) n = 4096;
    g_res = (uint32_t)n;
    return n;
}

/* vk_pipeline_depth(n): keep n frames in flight on the GPU (1..FRING_MAX). n==1 is the sound
 * default (each vk_draw returns THIS frame). n>1 pipelines: vk_draw returns the pixel from n-1
 * frames ago, and the trailing frames flush at exit — an OPT-IN for animations that IGNORE the
 * per-frame return (e.g. PPM output), where it hides GPU submit/fence latency. Must be called
 * before the first render (the ring is sized when the first frame's resources are built). */
int64_t jrt_vk_pipeline_depth(int64_t n) {
    if(n < 1) n = 1; if(n > FRING_MAX) n = FRING_MAX;
    g_pipe_depth = (int)n;
    return n;
}

/* vk_render3d(verts, idx, ca, sa): render 3D per-vertex-colored geometry (interleaved
 * x,y,z, r,g,b — 6 per vertex) to frame_NNN.ppm, with a rotation uniform {ca, sa, scale,
 * 1} the @vertex reads via uniform() to spin the mesh on the GPU. The 3D pipeline
 * back-face-culls (convex mesh → far side hidden without a depth buffer). Returns 0/-1. */
int64_t jrt_vk_render3d(const double *verts, int64_t nfloats, int64_t idx, double ca, double sa) {
    if(!verts || nfloats < 18 || (nfloats % 6)!=0) return -1;   /* >=3 (x,y,z,r,g,b) vertices */
    uint32_t nverts=(uint32_t)(nfloats/6);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    char path[64]; snprintf(path,sizeof path,"frame_%03d.ppm",(int)idx);
    float uni[4]={(float)ca,(float)sa,0.9f,1.0f};
    int64_t r=render_headless_d(f, nverts, 6, uni, path, 1);
    free(f);
    return r>=0 ? 0 : -1;
}

/* vk_draw(verts, ux,uy,uz,uw): the generic draw surface — the program supplies BOTH the
 * geometry (a flat [Float] array of interleaved (x,y), like vk_mesh) AND a vec4 uniform.
 * The runtime renders it through the compiled @vertex/@fragment shaders; the uniform is
 * pushed to the shader-declared push channel and read via `uniform()`. So "what to draw"
 * (geometry) and "with what parameters" (uniform) both come from the Vire program, and
 * the pipeline comes from its shaders — instead of a fixed per-demo entry point. Returns
 * the centroid pixel 0xRRGGBB (or -1). */
int64_t jrt_vk_draw(const double *verts, int64_t nfloats, double ux, double uy, double uz, double uw) {
    if(!verts || nfloats < 6 || (nfloats % 2)!=0) return -1;   /* >=3 (x,y) vertices */
    uint32_t nverts=(uint32_t)(nfloats/2);
    float *f=malloc((size_t)nfloats*sizeof(float)); if(!f) return -1;
    for(int64_t i=0;i<nfloats;i++) f[i]=(float)verts[i];
    float uni[4]={(float)ux,(float)uy,(float)uz,(float)uw};
    int64_t r=render_headless(f, nverts, 2, uni, 0);
    free(f);
    return r;
}

/* vk_mesh_scene(offsets): MANY meshlets from a Vire scene buffer. The N (x,y) offsets
 * are uploaded to an SSBO (binding 0); one indirect dispatch of N mesh workgroups
 * (vkCmdDrawMeshTasksIndirectEXT) draws N meshlets — each mesh workgroup reads its own
 * offset via meshlet_offset() (scene[gl_WorkGroupID.x]). Returns a 2-bit mask: bit 0 if
 * the left quarter is drawn, bit 1 if the right quarter is — so a caller can verify
 * multiple meshlets landed. -2 if no mesh-shader device, -1 on failure. */
/* Render a scene of meshlets. If `builder` is set, a @compute shader fills the scene
 * SSBO on the GPU first (bcount meshlets, `offs` ignored); otherwise the host offsets
 * are uploaded. `plane` is the frustum plane for the @task cull (permissive if none). */
static int64_t scene_render(const double *offs, int64_t nfloats, const float plane[4], int builder, uint32_t bcount, int readmode) {
    enum { W=256, H=256 };
    if(!builder && (!offs || nfloats < 2 || (nfloats & 1))) return -1;
    uint32_t nmesh = builder ? bcount : (uint32_t)(nfloats/2);
    if(nmesh==0) return -1;
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_3};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; CK(VK_CREATE_INSTANCE(&ici,&inst));
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

    /* Scene SSBO: N typed Meshlet records (std430: vec2 offset @0, vec2 cone @8,
     * vec4 color @16 — 32 bytes / 8 floats each), host-visible. The compute builder
     * fills it on the GPU (left uninitialized here); otherwise upload the host offsets
     * (cone + color left zero). */
    VkDeviceSize ssz=(VkDeviceSize)nmesh*8*sizeof(float); if(ssz==0) ssz=32;
    VkBufferCreateInfo sbi={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=ssz,.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer ssbo; CK(vkCreateBuffer(dev,&sbi,0,&ssbo));
    VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev,ssbo,&smr);
    uint32_t smt=find_mem(pd,smr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(smt==~0u) return -1;
    VkMemoryAllocateInfo sma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=smr.size,.memoryTypeIndex=smt};
    VkDeviceMemory smem; CK(vkAllocateMemory(dev,&sma,0,&smem)); vkBindBufferMemory(dev,ssbo,smem,0);
    if(!builder){
        float *rec=calloc((size_t)nmesh*8,sizeof(float)); if(!rec) return -1;
        for(uint32_t i=0;i<nmesh;i++){ rec[i*8+0]=(float)offs[i*2+0]; rec[i*8+1]=(float)offs[i*2+1]; } /* cone + color stay 0 */
        void *sp; CK(vkMapMemory(dev,smem,0,ssz,0,&sp)); memcpy(sp,rec,(size_t)nmesh*8*sizeof(float)); vkUnmapMemory(dev,smem); free(rec);
    }

    /* Descriptor set layout (binding 0 = SSBO). The task stage reads the scene when it
     * culls, and the compute builder writes it — include whichever stages exist. */
    /* the scene SSBO's descriptor-set layout — binding, type AND the stage mask are
     * REFLECTED from which shader stages actually read the scene buffer (V3), instead
     * of the hand-written `MESH | maybe TASK | maybe COMPUTE`. */
    VkDescriptorSetLayout dsl = mk_dsl_reflected(dev); if(!dsl) return -1;
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
    VkPipelineLayout pl; VkPipeline pipe=build_mesh_pipeline(dev,rp,W,H,&pl,dsl,0); if(!pipe) return -1;
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
    VkShaderStageFlags pcStages = VK_IFACE_PUSH_STAGES;  /* reflected: the stage that reads cull_plane() */
    vkCmdPushConstants(cmd,pl,pcStages,0,VK_IFACE_PUSH_SIZE,plane);   /* frustum plane for the @task cull */
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
    /* readmode 1: the left-quarter pixel packed 0xRRGGBB (to verify per-meshlet colour). */
    int64_t lcolor = ((int64_t)L[0]<<16)|((int64_t)L[1]<<8)|(int64_t)L[2];
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyBuffer(dev,ibuf,0); vkFreeMemory(dev,imem,0);
    vkDestroyDescriptorPool(dev,dpool,0); vkDestroyDescriptorSetLayout(dev,dsl,0);
    vkDestroyBuffer(dev,ssbo,0); vkFreeMemory(dev,smem,0);
    if(cpipe) vkDestroyPipeline(dev,cpipe,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0); vkDestroyFramebuffer(dev,fb,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return readmode==1 ? lcolor : mask;
}

/* vk_mesh_scene(offsets): many meshlets, no culling — a permissive plane. */
int64_t jrt_vk_mesh_scene(const double *offs, int64_t nfloats) {
    float permissive[4]={0.0f,0.0f,0.0f,1.0f};
    return scene_render(offs,nfloats,permissive,0,0,0);
}

/* vk_mesh_scene_cull(offsets, nx,ny,nz,d): the fused GPU-driven cull renderer. The
 * @task tests each meshlet's center against the pushed frustum plane and emits only
 * the survivors (payload carries the index); the @mesh draws them. */
int64_t jrt_vk_mesh_scene_cull(const double *offs, int64_t nfloats, double nx, double ny, double nz, double dd) {
    float plane[4]={(float)nx,(float)ny,(float)nz,(float)dd};
    return scene_render(offs,nfloats,plane,0,0,0);
}

/* vk_mesh_built(count, nx,ny,nz,d): the whole renderer is GPU-built. A @compute
 * builder fills the scene SSBO with `count` meshlets on the GPU (set_meshlet), then
 * the @task cull + @mesh draw run over it — the meshlet set never exists on the host.
 * Returns the same left|right coverage mask. */
int64_t jrt_vk_mesh_built(int64_t count, double nx, double ny, double nz, double dd) {
    if(count <= 0) return -1;
    float plane[4]={(float)nx,(float)ny,(float)nz,(float)dd};
    return scene_render(0,0,plane,1,(uint32_t)count,0);
}

/* vk_built_color(count, nx,ny,nz,d): like vk_mesh_built, but returns the LEFT-quarter
 * pixel colour packed 0xRRGGBB — for verifying per-meshlet colour (set_meshlet_color →
 * meshlet_rgb → fragment). */
int64_t jrt_vk_built_color(int64_t count, double nx, double ny, double nz, double dd) {
    if(count <= 0) return -1;
    float plane[4]={(float)nx,(float)ny,(float)nz,(float)dd};
    return scene_render(0,0,plane,1,(uint32_t)count,1);
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
    VkInstance inst; CK(VK_CREATE_INSTANCE(&ici,&inst));
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
    /* Depth attachment (D32) so overlapping meshlets occlude correctly. */
    VkImageCreateInfo di={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,.format=DEPTH_FMT,
        .extent={W,H,1},.mipLevels=1,.arrayLayers=1,.samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage dimg; CK(vkCreateImage(dev,&di,0,&dimg));
    VkMemoryRequirements dmr; vkGetImageMemoryRequirements(dev,dimg,&dmr);
    uint32_t dit=find_mem(pd,dmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(dit==~0u) return -1;
    VkMemoryAllocateInfo dma={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=dmr.size,.memoryTypeIndex=dit};
    VkDeviceMemory dmem; CK(vkAllocateMemory(dev,&dma,0,&dmem)); vkBindImageMemory(dev,dimg,dmem,0);
    VkImageViewCreateInfo dvi={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=dimg,.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=DEPTH_FMT,.subresourceRange={VK_IMAGE_ASPECT_DEPTH_BIT,0,1,0,1}};
    VkImageView dview; CK(vkCreateImageView(dev,&dvi,0,&dview));
    VkRenderPass rp=build_rp_d(dev,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,1); if(!rp) return -1;
    VkImageView atts[2]={view,dview};
    VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=2,.pAttachments=atts,.width=W,.height=H,.layers=1};
    VkFramebuffer fb; CK(vkCreateFramebuffer(dev,&fbi,0,&fb));
    VkPipelineLayout pl; VkPipeline pipe=build_mesh_pipeline(dev,rp,W,H,&pl,0,1); if(!pipe) return -1;

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
    VkClearValue clear[2]={{.color={{0.08f,0.08f,0.10f,1.0f}}},{.depthStencil={1.0f,0}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{W,H}},.clearValueCount=2,.pClearValues=clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    VkShaderStageFlags pcStages = VK_IFACE_PUSH_STAGES;  /* reflected: the stage that reads cull_plane() */
    vkCmdPushConstants(cmd,pl,pcStages,0,VK_IFACE_PUSH_SIZE,plane);   /* the frustum plane for @task cull */
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
    vkDestroyImageView(dev,dview,0); vkDestroyImage(dev,dimg,0); vkFreeMemory(dev,dmem,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return corner_clear ? packed : -1;
}

/* ---- windowed: open a window and present the triangle, ANIMATED — each frame's
 *      command buffer is re-recorded with a per-frame uniform (a moving colour), so a
 *      program whose @fragment reads uniform() shows an animation. Interactive windowed
 *      rendering. frames=0: until the window is closed. Returns 1, or 0 without a
 *      display/window. ---- */
static int64_t vk_window_impl(const float *verts, uint32_t nverts, int64_t frames) {
    if(!glfwInit()) return 0;
    if(!glfwVulkanSupported()){ glfwTerminate(); return 0; }
    glfwWindowHint(GLFW_CLIENT_API, GLFW_NO_API);
    GLFWwindow *win=glfwCreateWindow(800,600,"Vire @vulkan — triangle",0,0);
    if(!win){ glfwTerminate(); return 0; }

    uint32_t next=0; const char **ext=glfwGetRequiredInstanceExtensions(&next);
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app,.enabledExtensionCount=next,.ppEnabledExtensionNames=ext};
    VkInstance inst; CK(VK_CREATE_INSTANCE(&ici,&inst));
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
    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl,0,0); if(!pipe) return 0;
    VkBuffer vbuf; VkDeviceMemory vmem; if(!make_vbuf(dev,pd,verts,nverts*2,&vbuf,&vmem)) return 0;

    VkImageView *views=malloc(nimg*sizeof*views); VkFramebuffer *fbs=malloc(nimg*sizeof*fbs);
    VkCommandPoolCreateInfo cpi={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,.queueFamilyIndex=qf};
    VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBuffer *cmds=malloc(nimg*sizeof*cmds);
    VkCommandBufferAllocateInfo cai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=nimg};
    CK(vkAllocateCommandBuffers(dev,&cai,cmds));
    for(uint32_t i=0;i<nimg;i++){
        VkImageViewCreateInfo iv={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=imgs[i],.viewType=VK_IMAGE_VIEW_TYPE_2D,.format=sf.format,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
        CK(vkCreateImageView(dev,&iv,0,&views[i]));
        VkFramebufferCreateInfo fbi={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&views[i],.width=W,.height=H,.layers=1};
        CK(vkCreateFramebuffer(dev,&fbi,0,&fbs[i]));
        float uni[4]={0,0,0,0};
        rec_draw(cmds[i],rp,fbs[i],pipe,W,H,vbuf,nverts,pl,uni); CK(vkEndCommandBuffer(cmds[i]));
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
        /* animate: re-record the acquired image with a per-frame uniform (the @fragment
         * reads uniform()), so the window shows a moving colour — interactive rendering. */
        { float t=(float)(count%120)/120.0f; float uni[4]={t,1.0f-t,0.5f,1.0f};
          vkResetCommandBuffer(cmds[idx],0);
          rec_draw(cmds[idx],rp,fbs[idx],pipe,W,H,vbuf,nverts,pl,uni); vkEndCommandBuffer(cmds[idx]); }
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
/* vk_window(frames): the default animated triangle. */
int64_t jrt_vk_window(int64_t frames) { return vk_window_impl(DEFAULT_TRI, 3, frames); }
/* vk_window_mesh(verts, nfloats, frames): present ARBITRARY Vire geometry in the window —
 * `verts` is a flat [Float] of interleaved (x,y), f64, drawn as a triangle list and
 * animated (the @fragment reads uniform()). */
int64_t jrt_vk_window_mesh(const double *verts, int64_t nfloats, int64_t frames) {
    if(!verts || nfloats < 6 || (nfloats & 1)) return 0;
    uint32_t nverts=(uint32_t)(nfloats/2);
    float *xy=malloc((size_t)nfloats*sizeof(float)); if(!xy) return 0;
    for(int64_t i=0;i<nfloats;i++) xy[i]=(float)verts[i];
    int64_t r=vk_window_impl(xy, nverts, frames);
    free(xy);
    return r;
}
