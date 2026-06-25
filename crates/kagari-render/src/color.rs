//! Offscreen linear render target and the output-transform pass (specs §2.4).
//!
//! Compositing happens in an offscreen linear `Rgba16Float` target; the output
//! pass samples it into the (sRGB) swapchain, where the hardware performs the
//! linear→sRGB encode. This is the architectural backbone for HDR/wide-gamut
//! output later (§2.4 post-MVP), added non-destructively.

/// The offscreen linear compositing target.
pub(crate) struct OffscreenTarget {
    pub(crate) view: wgpu::TextureView,
}

impl OffscreenTarget {
    pub(crate) fn new(device: &wgpu::Device, (w, h): (u32, u32)) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kagari.offscreen.linear"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        // The view keeps the underlying texture alive, so we store only the view.
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { view }
    }
}

/// The output-transform pass: a cached pipeline + sampler that blits the offscreen
/// target onto the swapchain via a fullscreen triangle.
pub(crate) struct OutputTransform {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl OutputTransform {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kagari.output.shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/output.wgsl").into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.output.bind_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kagari.output.pipeline_layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kagari.output.pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kagari.output.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // Offscreen and swapchain are the same resolution, so nearest is exact.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            pipeline,
            bind_layout,
            sampler,
        }
    }

    /// Create a bind group for the current offscreen view (recreated on resize).
    pub(crate) fn bind(&self, device: &wgpu::Device, src: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.output.bind_group"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Record the fullscreen-triangle draw into an in-progress render pass.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, bind: &wgpu::BindGroup) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
