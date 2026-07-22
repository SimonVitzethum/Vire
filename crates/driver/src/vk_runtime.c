/* Vire @vulkan runtime (V2). Two entry points share one pipeline builder:
 *   jrt_vk_triangle()      — headless render + pixel self-verify (CI, no display).
 *   jrt_vk_window(frames)  — open a window, present the triangle (frames=0: until
 *                            closed). Needs a display + GLFW.
 * A compile-time-fixed graphics pipeline (positions from gl_VertexIndex, no vertex
 * buffers); bootstrap SPIR-V embedded below (glslc). The single-source Vire->SPIR-V
 * shader path + the `frame { clear; draw }` surface are the next milestones.
 * See language/GPU-VULKAN.md. Vendor-neutral (any Vulkan GPU).
 */
#define GLFW_INCLUDE_VULKAN
#include <GLFW/glfw3.h>
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

#define CK(x) do { if((x)!=VK_SUCCESS) return 0; } while(0)

static const uint32_t VK_TRI_VERT[] = {
  0x07230203,0x00010000,0x000d000b,0x00000028,0x00000000,0x00020011,0x00000001,0x0006000b,
  0x00000001,0x4c534c47,0x6474732e,0x3035342e,0x00000000,0x0003000e,0x00000000,0x00000001,
  0x0007000f,0x00000000,0x00000004,0x6e69616d,0x00000000,0x00000019,0x0000001d,0x00030003,
  0x00000002,0x000001c2,0x000a0004,0x475f4c47,0x4c474f4f,0x70635f45,0x74735f70,0x5f656c79,
  0x656e696c,0x7269645f,0x69746365,0x00006576,0x00080004,0x475f4c47,0x4c474f4f,0x6e695f45,
  0x64756c63,0x69645f65,0x74636572,0x00657669,0x00040005,0x00000004,0x6e69616d,0x00000000,
  0x00030005,0x0000000c,0x00000050,0x00060005,0x00000017,0x505f6c67,0x65567265,0x78657472,
  0x00000000,0x00060006,0x00000017,0x00000000,0x505f6c67,0x7469736f,0x006e6f69,0x00070006,
  0x00000017,0x00000001,0x505f6c67,0x746e696f,0x657a6953,0x00000000,0x00070006,0x00000017,
  0x00000002,0x435f6c67,0x4470696c,0x61747369,0x0065636e,0x00070006,0x00000017,0x00000003,
  0x435f6c67,0x446c6c75,0x61747369,0x0065636e,0x00030005,0x00000019,0x00000000,0x00060005,
  0x0000001d,0x565f6c67,0x65747265,0x646e4978,0x00007865,0x00030047,0x00000017,0x00000002,
  0x00050048,0x00000017,0x00000000,0x0000000b,0x00000000,0x00050048,0x00000017,0x00000001,
  0x0000000b,0x00000001,0x00050048,0x00000017,0x00000002,0x0000000b,0x00000003,0x00050048,
  0x00000017,0x00000003,0x0000000b,0x00000004,0x00040047,0x0000001d,0x0000000b,0x0000002a,
  0x00020013,0x00000002,0x00030021,0x00000003,0x00000002,0x00030016,0x00000006,0x00000020,
  0x00040017,0x00000007,0x00000006,0x00000002,0x00040015,0x00000008,0x00000020,0x00000000,
  0x0004002b,0x00000008,0x00000009,0x00000003,0x0004001c,0x0000000a,0x00000007,0x00000009,
  0x00040020,0x0000000b,0x00000006,0x0000000a,0x0004003b,0x0000000b,0x0000000c,0x00000006,
  0x0004002b,0x00000006,0x0000000d,0x00000000,0x0004002b,0x00000006,0x0000000e,0xbf19999a,
  0x0005002c,0x00000007,0x0000000f,0x0000000d,0x0000000e,0x0004002b,0x00000006,0x00000010,
  0x3f19999a,0x0005002c,0x00000007,0x00000011,0x00000010,0x00000010,0x0005002c,0x00000007,
  0x00000012,0x0000000e,0x00000010,0x0006002c,0x0000000a,0x00000013,0x0000000f,0x00000011,
  0x00000012,0x00040017,0x00000014,0x00000006,0x00000004,0x0004002b,0x00000008,0x00000015,
  0x00000001,0x0004001c,0x00000016,0x00000006,0x00000015,0x0006001e,0x00000017,0x00000014,
  0x00000006,0x00000016,0x00000016,0x00040020,0x00000018,0x00000003,0x00000017,0x0004003b,
  0x00000018,0x00000019,0x00000003,0x00040015,0x0000001a,0x00000020,0x00000001,0x0004002b,
  0x0000001a,0x0000001b,0x00000000,0x00040020,0x0000001c,0x00000001,0x0000001a,0x0004003b,
  0x0000001c,0x0000001d,0x00000001,0x00040020,0x0000001f,0x00000006,0x00000007,0x0004002b,
  0x00000006,0x00000022,0x3f800000,0x00040020,0x00000026,0x00000003,0x00000014,0x00050036,
  0x00000002,0x00000004,0x00000000,0x00000003,0x000200f8,0x00000005,0x0003003e,0x0000000c,
  0x00000013,0x0004003d,0x0000001a,0x0000001e,0x0000001d,0x00050041,0x0000001f,0x00000020,
  0x0000000c,0x0000001e,0x0004003d,0x00000007,0x00000021,0x00000020,0x00050051,0x00000006,
  0x00000023,0x00000021,0x00000000,0x00050051,0x00000006,0x00000024,0x00000021,0x00000001,
  0x00070050,0x00000014,0x00000025,0x00000023,0x00000024,0x0000000d,0x00000022,0x00050041,
  0x00000026,0x00000027,0x00000019,0x0000001b,0x0003003e,0x00000027,0x00000025,0x000100fd,
  0x00010038,
};

static const uint32_t VK_TRI_FRAG[] = {
  0x07230203,0x00010000,0x000d000b,0x0000000e,0x00000000,0x00020011,0x00000001,0x0006000b,
  0x00000001,0x4c534c47,0x6474732e,0x3035342e,0x00000000,0x0003000e,0x00000000,0x00000001,
  0x0006000f,0x00000004,0x00000004,0x6e69616d,0x00000000,0x00000009,0x00030010,0x00000004,
  0x00000007,0x00030003,0x00000002,0x000001c2,0x000a0004,0x475f4c47,0x4c474f4f,0x70635f45,
  0x74735f70,0x5f656c79,0x656e696c,0x7269645f,0x69746365,0x00006576,0x00080004,0x475f4c47,
  0x4c474f4f,0x6e695f45,0x64756c63,0x69645f65,0x74636572,0x00657669,0x00040005,0x00000004,
  0x6e69616d,0x00000000,0x00030005,0x00000009,0x00000063,0x00040047,0x00000009,0x0000001e,
  0x00000000,0x00020013,0x00000002,0x00030021,0x00000003,0x00000002,0x00030016,0x00000006,
  0x00000020,0x00040017,0x00000007,0x00000006,0x00000004,0x00040020,0x00000008,0x00000003,
  0x00000007,0x0004003b,0x00000008,0x00000009,0x00000003,0x0004002b,0x00000006,0x0000000a,
  0x3f800000,0x0004002b,0x00000006,0x0000000b,0x3ecccccd,0x0004002b,0x00000006,0x0000000c,
  0x3dcccccd,0x0007002c,0x00000007,0x0000000d,0x0000000a,0x0000000b,0x0000000c,0x0000000a,
  0x00050036,0x00000002,0x00000004,0x00000000,0x00000003,0x000200f8,0x00000005,0x0003003e,
  0x00000009,0x0000000d,0x000100fd,0x00010038,
};

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

/* The one shared piece: build the triangle graphics pipeline for a render pass +
 * extent. Layout is empty (no descriptors); shaders are the embedded SPIR-V. */
static VkPipeline build_pipeline(VkDevice dev, VkRenderPass rp, uint32_t w, uint32_t h, VkPipelineLayout *out_layout) {
    VkShaderModule vs=shmod(dev,VK_TRI_VERT,sizeof VK_TRI_VERT), fs=shmod(dev,VK_TRI_FRAG,sizeof VK_TRI_FRAG);
    if(!vs||!fs) return 0;
    VkPipelineShaderStageCreateInfo st[2]={
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_VERTEX_BIT,.module=vs,.pName="main"},
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_FRAGMENT_BIT,.module=fs,.pName="main"}};
    VkPipelineVertexInputStateCreateInfo vi={.sType=VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
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
static void rec_draw(VkCommandBuffer cmd, VkRenderPass rp, VkFramebuffer fb, VkPipeline pipe, uint32_t w, uint32_t h) {
    VkCommandBufferBeginInfo bi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(cmd,&bi);
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{w,h}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
}

/* ---- headless: render to an image, read back, self-verify ---- */
int jrt_vk_triangle(void) {
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
    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl); if(!pipe) return 0;

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
    rec_draw(cmd,rp,fb,pipe,W,H);
    VkBufferImageCopy rg={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={W,H,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&rg);
    CK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    CK(vkQueueSubmit(q,1,&si,fence)); CK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));
    unsigned char *px; CK(vkMapMemory(dev,bmem,0,W*H*4,0,(void**)&px));
    int cx=W/2, cy=(int)(H*0.55); unsigned char *c=&px[(cy*W+cx)*4], *tl=&px[(5*W+5)*4];
    int ok = c[0]>200&&c[1]>60&&c[1]<140&&c[2]<80 && tl[0]<60&&tl[1]<60&&tl[2]<60;
    vkUnmapMemory(dev,bmem);
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0); vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0); vkDestroyFramebuffer(dev,fb,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,im,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return ok?1:0;
}

/* ---- windowed: open a window and present the triangle (frames=0: until closed) ---- */
int jrt_vk_window(int64_t frames) {
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
    VkPipelineLayout pl; VkPipeline pipe=build_pipeline(dev,rp,W,H,&pl); if(!pipe) return 0;

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
        rec_draw(cmds[i],rp,fbs[i],pipe,W,H); CK(vkEndCommandBuffer(cmds[i]));
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
    vkDestroyCommandPool(dev,cp,0); vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyRenderPass(dev,rp,0); vkDestroySwapchainKHR(dev,sw,0); vkDestroyDevice(dev,0);
    vkDestroySurfaceKHR(inst,surf,0); vkDestroyInstance(inst,0);
    glfwDestroyWindow(win); glfwTerminate();
    free(imgs); free(views); free(fbs); free(cmds);
    return 1;
}
