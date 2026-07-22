// Hand-written Vulkan (Rust/ash) baseline for the @vulkan steady-state benchmark.
// Same workload as Vire's vk_bench and the C++ baseline: init once, render a
// mesh-shader triangle to a 256x256 headless image `frames` times (submit + fence
// wait per frame), print the per-frame nanoseconds. Loads the SAME SPIR-V the runner
// compiled (argv[2]=mesh.spv, argv[3]=frag.spv) so the GPU work is identical.
//
//   cargo build --release --offline && ./target/release/bench-ash 2000 mesh.spv frag.spv
use ash::vk;
use std::time::Instant;

const W: u32 = 256;
const H: u32 = 256;

fn load_spv(path: &str) -> Vec<u32> {
    let mut f = std::fs::File::open(path).unwrap_or_else(|_| panic!("open {path}"));
    ash::util::read_spv(&mut f).expect("read spv")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let frames: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let mesh_p = args.get(2).cloned().unwrap_or_else(|| "tri.mesh.spv".into());
    let frag_p = args.get(3).cloned().unwrap_or_else(|| "tri.frag.spv".into());

    unsafe {
        let entry = ash::Entry::load().expect("load vulkan");
        let app = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 3, 0));
        let ici = vk::InstanceCreateInfo::default().application_info(&app);
        let instance = entry.create_instance(&ici, None).expect("instance");

        // Pick a mesh-shader-capable device + graphics queue.
        let pds = instance.enumerate_physical_devices().unwrap();
        let mut chosen = None;
        for pd in pds {
            let exts = instance.enumerate_device_extension_properties(pd).unwrap();
            let has_mesh = exts.iter().any(|e| {
                e.extension_name_as_c_str().map(|c| c == ash::ext::mesh_shader::NAME).unwrap_or(false)
            });
            if !has_mesh { continue; }
            let qf = instance.get_physical_device_queue_family_properties(pd);
            if let Some(i) = qf.iter().position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS)) {
                chosen = Some((pd, i as u32));
                break;
            }
        }
        let (pd, qf) = match chosen { Some(x) => x, None => { eprintln!("no mesh-shader device"); std::process::exit(3); } };

        let prio = [1.0f32];
        let qci = vk::DeviceQueueCreateInfo::default().queue_family_index(qf).queue_priorities(&prio);
        let mut mesh_feat = vk::PhysicalDeviceMeshShaderFeaturesEXT::default().mesh_shader(true);
        let dext = [ash::ext::mesh_shader::NAME.as_ptr()];
        let qcis = [qci];
        let dci = vk::DeviceCreateInfo::default()
            .queue_create_infos(&qcis)
            .enabled_extension_names(&dext)
            .push_next(&mut mesh_feat);
        let device = instance.create_device(pd, &dci, None).expect("device");
        let queue = device.get_device_queue(qf, 0);
        let mesh_dev = ash::ext::mesh_shader::Device::new(&instance, &device);
        let memprops = instance.get_physical_device_memory_properties(pd);
        let find_mem = |bits: u32, want: vk::MemoryPropertyFlags| -> u32 {
            (0..memprops.memory_type_count).find(|&i| {
                (bits & (1 << i)) != 0 && memprops.memory_types[i as usize].property_flags.contains(want)
            }).expect("mem type")
        };

        // Color target + view.
        let ici2 = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D).format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: W, height: H, depth: 1 })
            .mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL).usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let img = device.create_image(&ici2, None).unwrap();
        let mr = device.get_image_memory_requirements(img);
        let mai = vk::MemoryAllocateInfo::default().allocation_size(mr.size)
            .memory_type_index(find_mem(mr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL));
        let imem = device.allocate_memory(&mai, None).unwrap();
        device.bind_image_memory(img, imem, 0).unwrap();
        let ivi = vk::ImageViewCreateInfo::default().image(img).view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1));
        let view = device.create_image_view(&ivi, None).unwrap();

        // Render pass + framebuffer.
        let att = vk::AttachmentDescription::default().format(vk::Format::R8G8B8A8_UNORM)
            .samples(vk::SampleCountFlags::TYPE_1).load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE).stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE).initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let atts = [att];
        let ref0 = [vk::AttachmentReference::default().attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let sub = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&ref0)];
        let rpci = vk::RenderPassCreateInfo::default().attachments(&atts).subpasses(&sub);
        let rp = device.create_render_pass(&rpci, None).unwrap();
        let views = [view];
        let fbi = vk::FramebufferCreateInfo::default().render_pass(rp).attachments(&views).width(W).height(H).layers(1);
        let fb = device.create_framebuffer(&fbi, None).unwrap();

        // Pipeline (mesh + fragment).
        let mw = load_spv(&mesh_p);
        let fw = load_spv(&frag_p);
        let ms = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&mw), None).unwrap();
        let fs = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&fw), None).unwrap();
        let main_c = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::MESH_EXT).module(ms).name(main_c),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs).name(main_c),
        ];
        let vps = [vk::Viewport { x: 0.0, y: 0.0, width: W as f32, height: H as f32, min_depth: 0.0, max_depth: 1.0 }];
        let scs = [vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: W, height: H } }];
        let vp = vk::PipelineViewportStateCreateInfo::default().viewports(&vps).scissors(&scs);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE).line_width(1.0);
        let msi = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let cba = [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
        let pl = device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), None).unwrap();
        let gp = vk::GraphicsPipelineCreateInfo::default().stages(&stages)
            .viewport_state(&vp).rasterization_state(&rs).multisample_state(&msi).color_blend_state(&cb)
            .layout(pl).render_pass(rp).subpass(0);
        let pipe = device.create_graphics_pipelines(vk::PipelineCache::null(), &[gp], None).unwrap()[0];

        // Command buffer (recorded once) + fence.
        let cp = device.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(qf), None).unwrap();
        let cmd = device.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
            .command_pool(cp).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1)).unwrap()[0];
        device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()).unwrap();
        let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.08, 0.08, 0.10, 1.0] } }];
        let rpbi = vk::RenderPassBeginInfo::default().render_pass(rp).framebuffer(fb)
            .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: W, height: H } })
            .clear_values(&clear);
        device.cmd_begin_render_pass(cmd, &rpbi, vk::SubpassContents::INLINE);
        device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
        mesh_dev.cmd_draw_mesh_tasks(cmd, 1, 1, 1);
        device.cmd_end_render_pass(cmd);
        device.end_command_buffer(cmd).unwrap();

        let fence = device.create_fence(&vk::FenceCreateInfo::default(), None).unwrap();
        let cmds = [cmd];
        let si = [vk::SubmitInfo::default().command_buffers(&cmds)];
        let submit_wait = || {
            device.queue_submit(queue, &si, fence).unwrap();
            device.wait_for_fences(&[fence], true, u64::MAX).unwrap();
            device.reset_fences(&[fence]).unwrap();
        };
        submit_wait(); // warm-up
        let t0 = Instant::now();
        for _ in 0..frames { submit_wait(); }
        let per = t0.elapsed().as_nanos() / frames as u128;
        println!("{per}");
    }
}
