/* Vire @vulkan runtime — V2 step 1: a headless, self-verifying triangle.
 *
 * Renders a triangle to an offscreen R8G8B8A8 image via a compile-time-fixed
 * graphics pipeline, reads the pixels back, and returns 1 iff the render is
 * correct (triangle color at the centroid, clear color at a corner) else 0.
 * This is the foundation of the @vulkan framework (see language/GPU-VULKAN.md):
 * instance/device/render-pass/pipeline/draw/readback, no windowing yet — so it
 * is CI-testable without a display and vendor-neutral (runs on any Vulkan GPU).
 * The shaders are bootstrap SPIR-V (glslc-compiled, embedded below); the
 * single-source Vire->SPIR-V shader path is the next milestone.
 */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

#define VKW 256
#define VKH 256
#define VKCK(x) do { if((x)!=VK_SUCCESS) return 0; } while(0)

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

static uint32_t vk_find_mem(VkPhysicalDevice pd, uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
    for(uint32_t i=0;i<mp.memoryTypeCount;i++)
        if((bits&(1u<<i)) && (mp.memoryTypes[i].propertyFlags&want)==want) return i;
    return ~0u;
}
static VkShaderModule vk_mod(VkDevice d, const uint32_t *code, size_t nbytes) {
    VkShaderModuleCreateInfo ci={.sType=VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,.codeSize=nbytes,.pCode=code};
    VkShaderModule m; if(vkCreateShaderModule(d,&ci,0,&m)!=VK_SUCCESS) return 0; return m;
}

/* Returns 1 if the triangle rendered correctly, else 0. */
int jrt_vk_triangle(void) {
    VkApplicationInfo app={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.apiVersion=VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&app};
    VkInstance inst; VKCK(vkCreateInstance(&ici,0,&inst));
    uint32_t ndev=0; vkEnumeratePhysicalDevices(inst,&ndev,0);
    if(!ndev) return 0;
    VkPhysicalDevice *pds=malloc(ndev*sizeof*pds); vkEnumeratePhysicalDevices(inst,&ndev,pds);
    VkPhysicalDevice pd=pds[0]; free(pds); uint32_t qfam=0;
    { uint32_t nq=0; vkGetPhysicalDeviceQueueFamilyProperties(pd,&nq,0);
      VkQueueFamilyProperties *qs=malloc(nq*sizeof*qs); vkGetPhysicalDeviceQueueFamilyProperties(pd,&nq,qs);
      int found=0; for(uint32_t i=0;i<nq;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){qfam=i;found=1;break;} free(qs);
      if(!found) return 0; }
    float prio=1.0f;
    VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qfam,.queueCount=1,.pQueuePriorities=&prio};
    VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci};
    VkDevice dev; VKCK(vkCreateDevice(pd,&dci,0,&dev));
    VkQueue q; vkGetDeviceQueue(dev,qfam,0,&q);

    VkImageCreateInfo imgci={.sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,.imageType=VK_IMAGE_TYPE_2D,
        .format=VK_FORMAT_R8G8B8A8_UNORM,.extent={VKW,VKH,1},.mipLevels=1,.arrayLayers=1,
        .samples=VK_SAMPLE_COUNT_1_BIT,.tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT,.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage img; VKCK(vkCreateImage(dev,&imgci,0,&img));
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,img,&mr);
    uint32_t it=vk_find_mem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); if(it==~0u) return 0;
    VkMemoryAllocateInfo mai={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=mr.size,.memoryTypeIndex=it};
    VkDeviceMemory imem; VKCK(vkAllocateMemory(dev,&mai,0,&imem)); vkBindImageMemory(dev,img,imem,0);
    VkImageViewCreateInfo ivci={.sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,.image=img,.viewType=VK_IMAGE_VIEW_TYPE_2D,
        .format=VK_FORMAT_R8G8B8A8_UNORM,.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1}};
    VkImageView view; VKCK(vkCreateImageView(dev,&ivci,0,&view));

    VkAttachmentDescription att={.format=VK_FORMAT_R8G8B8A8_UNORM,.samples=VK_SAMPLE_COUNT_1_BIT,
        .loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR,.storeOp=VK_ATTACHMENT_STORE_OP_STORE,
        .stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE,.stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE,
        .initialLayout=VK_IMAGE_LAYOUT_UNDEFINED,.finalLayout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL};
    VkAttachmentReference ref={0,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub={.pipelineBindPoint=VK_PIPELINE_BIND_POINT_GRAPHICS,.colorAttachmentCount=1,.pColorAttachments=&ref};
    VkRenderPassCreateInfo rpci={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,.attachmentCount=1,.pAttachments=&att,.subpassCount=1,.pSubpasses=&sub};
    VkRenderPass rp; VKCK(vkCreateRenderPass(dev,&rpci,0,&rp));
    VkFramebufferCreateInfo fbci={.sType=VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,.renderPass=rp,.attachmentCount=1,.pAttachments=&view,.width=VKW,.height=VKH,.layers=1};
    VkFramebuffer fb; VKCK(vkCreateFramebuffer(dev,&fbci,0,&fb));

    VkShaderModule vs=vk_mod(dev,VK_TRI_VERT,sizeof VK_TRI_VERT), fs=vk_mod(dev,VK_TRI_FRAG,sizeof VK_TRI_FRAG);
    if(!vs||!fs) return 0;
    VkPipelineShaderStageCreateInfo stages[2]={
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_VERTEX_BIT,.module=vs,.pName="main"},
        {.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,.stage=VK_SHADER_STAGE_FRAGMENT_BIT,.module=fs,.pName="main"}};
    VkPipelineVertexInputStateCreateInfo vi={.sType=VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    VkPipelineInputAssemblyStateCreateInfo ia={.sType=VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport vp={0,0,VKW,VKH,0,1}; VkRect2D scz={{0,0},{VKW,VKH}};
    VkPipelineViewportStateCreateInfo vps={.sType=VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,.viewportCount=1,.pViewports=&vp,.scissorCount=1,.pScissors=&scz};
    VkPipelineRasterizationStateCreateInfo rs={.sType=VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,.polygonMode=VK_POLYGON_MODE_FILL,.cullMode=VK_CULL_MODE_NONE,.frontFace=VK_FRONT_FACE_COUNTER_CLOCKWISE,.lineWidth=1.0f};
    VkPipelineMultisampleStateCreateInfo mss={.sType=VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState cba={.colorWriteMask=0xf};
    VkPipelineColorBlendStateCreateInfo cb={.sType=VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,.attachmentCount=1,.pAttachments=&cba};
    VkPipelineLayoutCreateInfo plci={.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    VkPipelineLayout pl; VKCK(vkCreatePipelineLayout(dev,&plci,0,&pl));
    VkGraphicsPipelineCreateInfo gp={.sType=VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,.stageCount=2,.pStages=stages,
        .pVertexInputState=&vi,.pInputAssemblyState=&ia,.pViewportState=&vps,.pRasterizationState=&rs,
        .pMultisampleState=&mss,.pColorBlendState=&cb,.layout=pl,.renderPass=rp,.subpass=0};
    VkPipeline pipe; VKCK(vkCreateGraphicsPipelines(dev,0,1,&gp,0,&pipe));

    VkBufferCreateInfo bci={.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,.size=VKW*VKH*4,.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf; VKCK(vkCreateBuffer(dev,&bci,0,&buf));
    VkMemoryRequirements bmr; vkGetBufferMemoryRequirements(dev,buf,&bmr);
    uint32_t bt=vk_find_mem(pd,bmr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT); if(bt==~0u) return 0;
    VkMemoryAllocateInfo bmai={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=bmr.size,.memoryTypeIndex=bt};
    VkDeviceMemory bmem; VKCK(vkAllocateMemory(dev,&bmai,0,&bmem)); vkBindBufferMemory(dev,buf,bmem,0);

    VkCommandPoolCreateInfo cpci={.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,.queueFamilyIndex=qfam};
    VkCommandPool cp; VKCK(vkCreateCommandPool(dev,&cpci,0,&cp));
    VkCommandBufferAllocateInfo cbai={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,.commandPool=cp,.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY,.commandBufferCount=1};
    VkCommandBuffer cmd; VKCK(vkAllocateCommandBuffers(dev,&cbai,&cmd));
    VkCommandBufferBeginInfo cbbi={.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VKCK(vkBeginCommandBuffer(cmd,&cbbi));
    VkClearValue clear={.color={{0.08f,0.08f,0.10f,1.0f}}};
    VkRenderPassBeginInfo rpbi={.sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,.renderPass=rp,.framebuffer=fb,.renderArea={{0,0},{VKW,VKH}},.clearValueCount=1,.pClearValues=&clear};
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdDraw(cmd,3,1,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region={.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1},.imageExtent={VKW,VKH,1}};
    vkCmdCopyImageToBuffer(cmd,img,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,buf,1,&region);
    VKCK(vkEndCommandBuffer(cmd));
    VkFenceCreateInfo fci={.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence; VKCK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si={.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO,.commandBufferCount=1,.pCommandBuffers=&cmd};
    VKCK(vkQueueSubmit(q,1,&si,fence));
    VKCK(vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull));

    unsigned char *px; VKCK(vkMapMemory(dev,bmem,0,VKW*VKH*4,0,(void**)&px));
    /* Self-verify: centroid = triangle color (orange ~255,102,25), a top corner = clear (dark). */
    int cx=VKW/2, cy=(int)(VKH*0.55);
    unsigned char *c=&px[(cy*VKW+cx)*4], *tl=&px[(5*VKW+5)*4];
    int ok = c[0]>200 && c[1]>60 && c[1]<140 && c[2]<80        /* orange fill */
          && tl[0]<60 && tl[1]<60 && tl[2]<60;                  /* dark clear corner */
    vkUnmapMemory(dev,bmem);
    /* Teardown (order: device idle already guaranteed by the fence wait). */
    vkDestroyFence(dev,fence,0); vkDestroyCommandPool(dev,cp,0);
    vkDestroyBuffer(dev,buf,0); vkFreeMemory(dev,bmem,0);
    vkDestroyPipeline(dev,pipe,0); vkDestroyPipelineLayout(dev,pl,0);
    vkDestroyShaderModule(dev,vs,0); vkDestroyShaderModule(dev,fs,0);
    vkDestroyFramebuffer(dev,fb,0); vkDestroyRenderPass(dev,rp,0);
    vkDestroyImageView(dev,view,0); vkDestroyImage(dev,img,0); vkFreeMemory(dev,imem,0);
    vkDestroyDevice(dev,0); vkDestroyInstance(inst,0);
    return ok ? 1 : 0;
}
