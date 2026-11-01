//! GPU side: pipelines, offscreen targets and the egui paint callback.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

use crate::mesh;

pub const MAX_POOLS: usize = 64;
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MAX_BLOOM_MIPS: usize = 6;

/// Per-gift GPU data (instance buffer and floor-pool uniform array). 96 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub struct GpuGift {
    /// x, z (metres), yaw, emission gain.
    pub pos_yaw: [f32; 4],
    /// Linear HDR LED colours.
    pub leds: [[f32; 4]; 4],
    /// online, selected, hovered, dragging.
    pub flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    pub target: [f32; 4],
    pub grid: [f32; 4],
    pub accent: [f32; 4],
    pub fog: [f32; 4],
    pub gifts: [GpuGift; MAX_POOLS],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub struct PostUniform {
    pub uv_rect: [f32; 4],
    pub params: [f32; 4],
    pub params2: [f32; 4],
}

/// Everything one frame of one stage needs, captured by value in the callback.
pub struct FrameData {
    pub stage_id: u64,
    pub alive: Arc<AtomicBool>,
    /// Widget rect in points.
    pub rect: egui::Rect,
    pub globals: Box<Globals>,
    pub gifts: Vec<GpuGift>,
    pub post: PostUniform,
}

pub struct StageCallback(pub FrameData);

/// Shared pipelines (one per app), stored in egui_wgpu's callback resources.
pub struct StageRenderer {
    box_pipeline: wgpu::RenderPipeline,
    reflect_pipeline: wgpu::RenderPipeline,
    floor_pipeline: wgpu::RenderPipeline,
    down_first_pipeline: wgpu::RenderPipeline,
    down_pipeline: wgpu::RenderPipeline,
    up_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    globals_layout: wgpu::BindGroupLayout,
    post_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    msaa: u32,
    target_is_srgb: bool,
    max_dim: u32,
    instances: HashMap<u64, StageInstance>,
}

/// Per-stage-widget GPU state (buffers and size-dependent targets).
struct StageInstance {
    alive: Arc<AtomicBool>,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    post_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    instance_cap: usize,
    targets: Option<Targets>,
}

struct Targets {
    size: [u32; 2],
    msaa_view: Option<wgpu::TextureView>,
    depth_view: wgpu::TextureView,
    hdr_view: wgpu::TextureView,
    mip_views: Vec<wgpu::TextureView>,
    down_bgs: Vec<wgpu::BindGroup>,
    up_bgs: Vec<wgpu::BindGroup>,
    composite_bg: wgpu::BindGroup,
}

/// Options describing egui's own render pass (so the composite pipeline matches it).
#[derive(Clone, Copy, Debug)]
pub struct TargetInfo {
    pub msaa_samples: u32,
    pub depth_format: Option<wgpu::TextureFormat>,
}

impl StageRenderer {
    pub fn new(
        device: &wgpu::Device,
        adapter: &wgpu::Adapter,
        target_format: wgpu::TextureFormat,
        info: TargetInfo,
    ) -> Self {
        let scene_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lumigift_stage_scene"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene.wgsl").into()),
        });
        let post_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lumigift_stage_post"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/post.wgsl").into()),
        });

        let msaa = {
            let f = adapter.get_texture_format_features(HDR_FORMAT);
            if f.flags.sample_count_supported(4)
                && f.flags
                    .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE)
            {
                4
            } else {
                1
            }
        };

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumigift_stage_globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<Globals>() as u64),
                },
                count: None,
            }],
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let post_entries = [
            tex_entry(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<PostUniform>() as u64),
                },
                count: None,
            },
        ];
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumigift_stage_post"),
            entries: &post_entries,
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumigift_stage_composite"),
            entries: &[
                post_entries[0],
                post_entries[1],
                post_entries[2],
                tex_entry(3),
            ],
        });

        let scene_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lumigift_stage_scene"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });
        let post_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lumigift_stage_post"),
            bind_group_layouts: &[Some(&post_layout)],
            immediate_size: 0,
        });
        let composite_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lumigift_stage_composite"),
            bind_group_layouts: &[Some(&composite_layout)],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<mesh::Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x3],
        };
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<GpuGift>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4, 8 => Float32x4
            ],
        };

        let scene_depth = Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });
        let ms = wgpu::MultisampleState {
            count: msaa,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };

        let scene_pipeline = |label: &str,
                              vs: &str,
                              fs: &str,
                              buffers: &[Option<wgpu::VertexBufferLayout>],
                              primitive: wgpu::PrimitiveState,
                              blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&scene_pl),
                vertex: wgpu::VertexState {
                    module: &scene_module,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: Default::default(),
                },
                primitive,
                depth_stencil: scene_depth.clone(),
                multisample: ms,
                fragment: Some(wgpu::FragmentState {
                    module: &scene_module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let mesh_buffers = [Some(vertex_layout), Some(instance_layout)];
        let culled = |front_face| wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        };
        let box_pipeline = scene_pipeline(
            "lumigift_stage_box",
            "vs_box",
            "fs_box",
            &mesh_buffers,
            culled(wgpu::FrontFace::Ccw),
            None,
        );
        // Mirrored in y, so the winding flips.
        let reflect_pipeline = scene_pipeline(
            "lumigift_stage_reflect",
            "vs_reflect",
            "fs_reflect",
            &mesh_buffers,
            culled(wgpu::FrontFace::Cw),
            None,
        );
        let floor_pipeline = scene_pipeline(
            "lumigift_stage_floor",
            "vs_floor",
            "fs_floor",
            &[],
            wgpu::PrimitiveState::default(),
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        let fullscreen = |label: &str,
                          layout: &wgpu::PipelineLayout,
                          fs: &str,
                          format: wgpu::TextureFormat,
                          blend: Option<wgpu::BlendState>,
                          depth: Option<wgpu::DepthStencilState>,
                          samples: u32| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &post_module,
                    entry_point: Some("vs_fullscreen"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth,
                multisample: wgpu::MultisampleState {
                    count: samples,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &post_module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };
        let down_first_pipeline = fullscreen(
            "lumigift_stage_bloom_down0",
            &post_pl,
            "fs_down_first",
            HDR_FORMAT,
            None,
            None,
            1,
        );
        let down_pipeline = fullscreen(
            "lumigift_stage_bloom_down",
            &post_pl,
            "fs_down",
            HDR_FORMAT,
            None,
            None,
            1,
        );
        let up_pipeline = fullscreen(
            "lumigift_stage_bloom_up",
            &post_pl,
            "fs_up",
            HDR_FORMAT,
            Some(additive),
            None,
            1,
        );
        let composite_depth = info.depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });
        let composite_pipeline = fullscreen(
            "lumigift_stage_composite",
            &composite_pl,
            "fs_composite",
            target_format,
            None,
            composite_depth,
            info.msaa_samples.max(1),
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lumigift_stage_linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let m = mesh::gift_mesh();
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lumigift_stage_gift_vertices"),
            contents: bytemuck::cast_slice(&m.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lumigift_stage_gift_indices"),
            contents: bytemuck::cast_slice(&m.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Self {
            box_pipeline,
            reflect_pipeline,
            floor_pipeline,
            down_first_pipeline,
            down_pipeline,
            up_pipeline,
            composite_pipeline,
            globals_layout,
            post_layout,
            composite_layout,
            sampler,
            vertex_buffer,
            index_buffer,
            index_count: m.indices.len() as u32,
            msaa,
            target_is_srgb: target_format.is_srgb(),
            max_dim: device.limits().max_texture_dimension_2d,
            instances: HashMap::new(),
        }
    }
}

impl StageInstance {
    fn new(device: &wgpu::Device, globals_layout: &wgpu::BindGroupLayout, alive: &Arc<AtomicBool>) -> Self {
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumigift_stage_globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumigift_stage_globals"),
            layout: globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        let post_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumigift_stage_post"),
            size: size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_cap = 32;
        Self {
            alive: alive.clone(),
            globals_buf,
            globals_bg,
            post_buf,
            instance_buf: create_instance_buffer(device, instance_cap),
            instance_cap,
            targets: None,
        }
    }
}

fn create_instance_buffer(device: &wgpu::Device, cap: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lumigift_stage_instances"),
        size: (cap * size_of::<GpuGift>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Number of bloom mips for a given size (each mip at least 4 px on its short side).
fn bloom_mips(size: [u32; 2]) -> usize {
    let short = size[0].min(size[1]).max(1);
    // Halvings of `short` that stay >= 4 px: floor(log2(short / 4)).
    (short.ilog2() as usize).saturating_sub(2).clamp(1, MAX_BLOOM_MIPS)
}

fn create_targets(r: &StageRenderer, device: &wgpu::Device, size: [u32; 2], post_buf: &wgpu::Buffer) -> Targets {
    let tex = |label: &str, w: u32, h: u32, format, samples, usage| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w.max(1),
                    height: h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    };
    let [w, h] = size;
    let rt = wgpu::TextureUsages::RENDER_ATTACHMENT;
    let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    let msaa_view = (r.msaa > 1).then(|| tex("lumigift_stage_msaa", w, h, HDR_FORMAT, r.msaa, rt));
    let depth_view = tex("lumigift_stage_depth", w, h, DEPTH_FORMAT, r.msaa, rt);
    let hdr_view = tex("lumigift_stage_hdr", w, h, HDR_FORMAT, 1, sampled);
    let n = bloom_mips(size);
    let mip_views: Vec<_> = (0..n)
        .map(|i| {
            let s = i as u32 + 1;
            tex("lumigift_stage_bloom", (w >> s).max(1), (h >> s).max(1), HDR_FORMAT, 1, sampled)
        })
        .collect();
    // Bindings 0..=2 of both the post and the composite layout.
    let post_entries = |src| {
        [
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&r.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: post_buf.as_entire_binding(),
            },
        ]
    };
    let post_bg = |src| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumigift_stage_post"),
            layout: &r.post_layout,
            entries: &post_entries(src),
        })
    };
    let down_bgs = (0..n)
        .map(|i| post_bg(if i == 0 { &hdr_view } else { &mip_views[i - 1] }))
        .collect();
    // up_bgs[i] samples mip i+1 and is drawn into mip i.
    let up_bgs = (0..n.saturating_sub(1)).map(|i| post_bg(&mip_views[i + 1])).collect();
    let [e0, e1, e2] = post_entries(&hdr_view);
    let composite_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lumigift_stage_composite"),
        layout: &r.composite_layout,
        entries: &[
            e0,
            e1,
            e2,
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&mip_views[0]),
            },
        ],
    });
    Targets {
        size,
        msaa_view,
        depth_view,
        hdr_view,
        mip_views,
        down_bgs,
        up_bgs,
        composite_bg,
    }
}

/// Integer pixel rect of the widget, unclamped (left, top, width, height).
fn rect_px(rect: egui::Rect, ppp: f32) -> (i32, i32, i32, i32) {
    let l = (rect.min.x * ppp).round() as i32;
    let t = (rect.min.y * ppp).round() as i32;
    let r = (rect.max.x * ppp).round() as i32;
    let b = (rect.max.y * ppp).round() as i32;
    (l, t, (r - l).max(0), (b - t).max(0))
}

impl egui_wgpu::CallbackTrait for StageCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = resources.get_mut::<StageRenderer>() else {
            return Vec::new();
        };
        let f = &self.0;
        let ppp = screen.pixels_per_point;
        let (l, t, w, h) = rect_px(f.rect, ppp);
        if w <= 0 || h <= 0 {
            return Vec::new();
        }
        let size = [(w as u32).min(r.max_dim), (h as u32).min(r.max_dim)];

        // Visible (clamped) part of the widget, as egui will set the viewport.
        let vp = egui::epaint::ViewportInPixels::from_points(&f.rect, ppp, screen.size_in_pixels);
        let mut post = f.post;
        post.uv_rect = [
            (vp.left_px - l) as f32 / w as f32,
            (vp.top_px - t) as f32 / h as f32,
            vp.width_px as f32 / w as f32,
            vp.height_px as f32 / h as f32,
        ];
        post.params2[0] = if r.target_is_srgb { 1.0 } else { 0.0 };

        // Drop GPU state of stages that no longer exist, then take this
        // stage's state out of the map while recording (put back at the end).
        r.instances.retain(|_, inst| inst.alive.load(Ordering::Relaxed));
        let mut inst = match r.instances.remove(&f.stage_id) {
            Some(inst) => inst,
            None => StageInstance::new(device, &r.globals_layout, &f.alive),
        };
        if inst.targets.as_ref().is_none_or(|tg| tg.size != size) {
            inst.targets = Some(create_targets(r, device, size, &inst.post_buf));
        }
        let tg = inst.targets.as_ref().unwrap();
        post.params2[3] = tg.mip_views.len() as f32;

        // Upload per-frame data.
        if f.gifts.len() > inst.instance_cap {
            inst.instance_cap = f.gifts.len().next_power_of_two();
            inst.instance_buf = create_instance_buffer(device, inst.instance_cap);
        }
        if !f.gifts.is_empty() {
            queue.write_buffer(&inst.instance_buf, 0, bytemuck::cast_slice(&f.gifts));
        }
        let instance_count = f.gifts.len() as u32;
        queue.write_buffer(&inst.globals_buf, 0, bytemuck::bytes_of(f.globals.as_ref()));
        queue.write_buffer(&inst.post_buf, 0, bytemuck::bytes_of(&post));

        let stage_bg = crate::srgb_to_linear_color(crate::STAGE_BG);

        // 1. Scene pass: reflections, floor (blended), boxes.
        {
            let (view, resolve) = match &tg.msaa_view {
                Some(m) => (m, Some(&tg.hdr_view)),
                None => (&tg.hdr_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumigift_stage_scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: stage_bg[0] as f64,
                            g: stage_bg[1] as f64,
                            b: stage_bg[2] as f64,
                            a: 1.0,
                        }),
                        store: if resolve.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &tg.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &inst.globals_bg, &[]);
            if instance_count > 0 {
                pass.set_vertex_buffer(0, r.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, inst.instance_buf.slice(..));
                pass.set_index_buffer(r.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.set_pipeline(&r.reflect_pipeline);
                pass.draw_indexed(0..r.index_count, 0, 0..instance_count);
            }
            pass.set_pipeline(&r.floor_pipeline);
            pass.draw(0..6, 0..1);
            if instance_count > 0 {
                pass.set_pipeline(&r.box_pipeline);
                pass.draw_indexed(0..r.index_count, 0, 0..instance_count);
            }
        }

        // 2. Bloom: downsample chain, then additive tent upsample back up.
        let fullscreen_pass = |encoder: &mut wgpu::CommandEncoder,
                               target: &wgpu::TextureView,
                               load: wgpu::LoadOp<wgpu::Color>,
                               pipeline: &wgpu::RenderPipeline,
                               bg: &wgpu::BindGroup| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumigift_stage_bloom"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..3, 0..1);
        };
        for i in 0..tg.mip_views.len() {
            let pipeline = if i == 0 {
                &r.down_first_pipeline
            } else {
                &r.down_pipeline
            };
            fullscreen_pass(
                encoder,
                &tg.mip_views[i],
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                pipeline,
                &tg.down_bgs[i],
            );
        }
        for i in (0..tg.up_bgs.len()).rev() {
            fullscreen_pass(
                encoder,
                &tg.mip_views[i],
                wgpu::LoadOp::Load,
                &r.up_pipeline,
                &tg.up_bgs[i],
            );
        }
        r.instances.insert(f.stage_id, inst);
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(r) = resources.get::<StageRenderer>() else {
            return;
        };
        let Some(inst) = r.instances.get(&self.0.stage_id) else {
            return;
        };
        let Some(tg) = &inst.targets else {
            return;
        };
        let vp = info.viewport_in_pixels();
        if vp.width_px <= 0 || vp.height_px <= 0 {
            return;
        }
        render_pass.set_pipeline(&r.composite_pipeline);
        render_pass.set_bind_group(0, &tg.composite_bg, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    fn validate(src: &str) {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn shaders_validate() {
        validate(include_str!("shaders/scene.wgsl"));
        validate(include_str!("shaders/post.wgsl"));
    }

    #[test]
    fn struct_sizes_match_wgsl() {
        assert_eq!(size_of::<super::GpuGift>(), 96);
        assert_eq!(size_of::<super::Globals>(), 64 + 5 * 16 + 96 * super::MAX_POOLS);
        assert_eq!(size_of::<super::PostUniform>(), 48);
    }
}
