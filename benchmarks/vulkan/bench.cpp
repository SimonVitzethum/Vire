// Hand-written Vulkan (C++/vulkan.h) baseline for the @vulkan steady-state benchmark.
// Same workload as Vire's vk_bench and the Rust/ash baseline: init once, render a
// mesh-shader triangle to a 256x256 headless image `frames` times (submit + fence
// wait per frame), print the per-frame nanoseconds. Loads the SAME SPIR-V the runner
// compiled (argv[1]=mesh.spv, argv[2]=frag.spv), so the GPU work is identical across
// all three languages — this isolates the CPU-side submission cost.
//
//   g++ -O2 bench.cpp -lvulkan -o bench && ./bench 2000 mesh.spv frag.spv
#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <cstring>
#include <vector>
#include <ctime>
#define CK(x) do{ if((x)!=VK_SUCCESS){ std::fprintf(stderr,"vk fail %d @ %d\n",(x),__LINE__); return 2; } }while(0)
static int64_t now_ns(){ timespec t; clock_gettime(CLOCK_MONOTONIC,&t); return (int64_t)t.tv_sec*1000000000LL+t.tv_nsec; }
static std::vector<uint32_t> load(const char*p){ FILE*f=fopen(p,"rb"); if(!f){std::perror(p);exit(2);} fseek(f,0,SEEK_END); long n=ftell(f); fseek(f,0,SEEK_SET); std::vector<uint32_t> v(n/4); if(fread(v.data(),1,n,f)!=(size_t)n){exit(2);} fclose(f); return v; }
static uint32_t findMem(VkPhysicalDevice pd,uint32_t bits,VkMemoryPropertyFlags w){ VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp); for(uint32_t i=0;i<mp.memoryTypeCount;i++) if((bits&(1u<<i))&&(mp.memoryTypes[i].propertyFlags&w)==w) return i; return ~0u; }

int main(int argc,char**argv){
    int frames = argc>1? atoi(argv[1]) : 2000;
    const char* meshp = argc>2? argv[2] : "tri.mesh.spv";
    const char* fragp = argc>3? argv[3] : "tri.frag.spv";
    enum { W=256, H=256 };
    VkApplicationInfo app{VK_STRUCTURE_TYPE_APPLICATION_INFO}; app.apiVersion=VK_API_VERSION_1_3;
    VkInstanceCreateInfo ici{VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO}; ici.pApplicationInfo=&app;
    VkInstance inst; CK(vkCreateInstance(&ici,0,&inst));
    uint32_t nd=0; vkEnumeratePhysicalDevices(inst,&nd,0);
    std::vector<VkPhysicalDevice> pds(nd); vkEnumeratePhysicalDevices(inst,&nd,pds.data());
    VkPhysicalDevice pd=0; uint32_t qf=0;
    for(auto d: pds){
        uint32_t ec=0; vkEnumerateDeviceExtensionProperties(d,0,&ec,0);
        std::vector<VkExtensionProperties> ex(ec); vkEnumerateDeviceExtensionProperties(d,0,&ec,ex.data());
        bool mesh=false; for(auto&e:ex) if(!strcmp(e.extensionName,VK_EXT_MESH_SHADER_EXTENSION_NAME)) mesh=true;
        if(!mesh) continue;
        uint32_t n=0; vkGetPhysicalDeviceQueueFamilyProperties(d,&n,0);
        std::vector<VkQueueFamilyProperties> qs(n); vkGetPhysicalDeviceQueueFamilyProperties(d,&n,qs.data());
        for(uint32_t i=0;i<n;i++) if(qs[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){ pd=d; qf=i; break; }
        if(pd) break;
    }
    if(!pd){ std::fprintf(stderr,"no mesh-shader device\n"); return 3; }
    VkPhysicalDeviceMeshShaderFeaturesEXT mf{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MESH_SHADER_FEATURES_EXT}; mf.meshShader=VK_TRUE;
    const char* dext[]={VK_EXT_MESH_SHADER_EXTENSION_NAME};
    float pr=1; VkDeviceQueueCreateInfo qci{VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO}; qci.queueFamilyIndex=qf; qci.queueCount=1; qci.pQueuePriorities=&pr;
    VkDeviceCreateInfo dci{VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO}; dci.pNext=&mf; dci.queueCreateInfoCount=1; dci.pQueueCreateInfos=&qci; dci.enabledExtensionCount=1; dci.ppEnabledExtensionNames=dext;
    VkDevice dev; CK(vkCreateDevice(pd,&dci,0,&dev)); VkQueue q; vkGetDeviceQueue(dev,qf,0,&q);
    auto drawMesh=(PFN_vkCmdDrawMeshTasksEXT)vkGetDeviceProcAddr(dev,"vkCmdDrawMeshTasksEXT");
    if(!drawMesh){ std::fprintf(stderr,"no vkCmdDrawMeshTasksEXT\n"); return 3; }

    VkImageCreateInfo ii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; ii.imageType=VK_IMAGE_TYPE_2D; ii.format=VK_FORMAT_R8G8B8A8_UNORM; ii.extent={W,H,1}; ii.mipLevels=1; ii.arrayLayers=1; ii.samples=VK_SAMPLE_COUNT_1_BIT; ii.tiling=VK_IMAGE_TILING_OPTIMAL; ii.usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT; ii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
    VkImage img; CK(vkCreateImage(dev,&ii,0,&img));
    VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev,img,&mr);
    VkMemoryAllocateInfo ma{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; ma.allocationSize=mr.size; ma.memoryTypeIndex=findMem(pd,mr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    VkDeviceMemory im; CK(vkAllocateMemory(dev,&ma,0,&im)); vkBindImageMemory(dev,img,im,0);
    VkImageViewCreateInfo ivi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; ivi.image=img; ivi.viewType=VK_IMAGE_VIEW_TYPE_2D; ivi.format=VK_FORMAT_R8G8B8A8_UNORM; ivi.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
    VkImageView view; CK(vkCreateImageView(dev,&ivi,0,&view));

    VkAttachmentDescription att{}; att.format=VK_FORMAT_R8G8B8A8_UNORM; att.samples=VK_SAMPLE_COUNT_1_BIT; att.loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR; att.storeOp=VK_ATTACHMENT_STORE_OP_STORE; att.stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE; att.stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE; att.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED; att.finalLayout=VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    VkAttachmentReference ref{0,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub{}; sub.pipelineBindPoint=VK_PIPELINE_BIND_POINT_GRAPHICS; sub.colorAttachmentCount=1; sub.pColorAttachments=&ref;
    VkRenderPassCreateInfo rpci{VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO}; rpci.attachmentCount=1; rpci.pAttachments=&att; rpci.subpassCount=1; rpci.pSubpasses=&sub;
    VkRenderPass rp; CK(vkCreateRenderPass(dev,&rpci,0,&rp));
    VkFramebufferCreateInfo fbi{VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO}; fbi.renderPass=rp; fbi.attachmentCount=1; fbi.pAttachments=&view; fbi.width=W; fbi.height=H; fbi.layers=1;
    VkFramebuffer fb; CK(vkCreateFramebuffer(dev,&fbi,0,&fb));

    auto mw=load(meshp), fw=load(fragp);
    VkShaderModuleCreateInfo msci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; msci.codeSize=mw.size()*4; msci.pCode=mw.data();
    VkShaderModuleCreateInfo fsci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; fsci.codeSize=fw.size()*4; fsci.pCode=fw.data();
    VkShaderModule ms,fs; CK(vkCreateShaderModule(dev,&msci,0,&ms)); CK(vkCreateShaderModule(dev,&fsci,0,&fs));
    VkPipelineShaderStageCreateInfo st[2]={{VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO},{VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}};
    st[0].stage=VK_SHADER_STAGE_MESH_BIT_EXT; st[0].module=ms; st[0].pName="main";
    st[1].stage=VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module=fs; st[1].pName="main";
    VkViewport vp{0,0,(float)W,(float)H,0,1}; VkRect2D sc{{0,0},{W,H}};
    VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO}; vps.viewportCount=1; vps.pViewports=&vp; vps.scissorCount=1; vps.pScissors=&sc;
    VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO}; rs.polygonMode=VK_POLYGON_MODE_FILL; rs.cullMode=VK_CULL_MODE_NONE; rs.frontFace=VK_FRONT_FACE_COUNTER_CLOCKWISE; rs.lineWidth=1;
    VkPipelineMultisampleStateCreateInfo msi{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO}; msi.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT;
    VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask=0xf;
    VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO}; cb.attachmentCount=1; cb.pAttachments=&cba;
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; VkPipelineLayout pl; CK(vkCreatePipelineLayout(dev,&plci,0,&pl));
    VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO}; gp.stageCount=2; gp.pStages=st; gp.pViewportState=&vps; gp.pRasterizationState=&rs; gp.pMultisampleState=&msi; gp.pColorBlendState=&cb; gp.layout=pl; gp.renderPass=rp; gp.subpass=0;
    VkPipeline pipe; CK(vkCreateGraphicsPipelines(dev,0,1,&gp,0,&pipe));

    VkCommandPoolCreateInfo cpi{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO}; cpi.queueFamilyIndex=qf; VkCommandPool cp; CK(vkCreateCommandPool(dev,&cpi,0,&cp));
    VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=cp; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1; VkCommandBuffer cmd; CK(vkAllocateCommandBuffers(dev,&cai,&cmd));
    VkCommandBufferBeginInfo cbi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; vkBeginCommandBuffer(cmd,&cbi);
    VkClearValue clear{}; clear.color={{0.08f,0.08f,0.10f,1.0f}};
    VkRenderPassBeginInfo rpbi{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpbi.renderPass=rp; rpbi.framebuffer=fb; rpbi.renderArea={{0,0},{W,H}}; rpbi.clearValueCount=1; rpbi.pClearValues=&clear;
    vkCmdBeginRenderPass(cmd,&rpbi,VK_SUBPASS_CONTENTS_INLINE); vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe); drawMesh(cmd,1,1,1); vkCmdEndRenderPass(cmd); CK(vkEndCommandBuffer(cmd));

    VkFenceCreateInfo fci{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; CK(vkCreateFence(dev,&fci,0,&fence));
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull); vkResetFences(dev,1,&fence); // warm-up
    int64_t t0=now_ns();
    for(int f=0;f<frames;f++){ vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,~0ull); vkResetFences(dev,1,&fence); }
    int64_t elapsed=now_ns()-t0;
    std::printf("%lld\n",(long long)(elapsed/frames));
    return 0;
}
