//! WebGPU adapter for native baked block models. Lighting, biome tint,
//! animated sprites, fluids and entities remain separate porting work.
use crate::{
    resources::{Atlas, Resources, direction},
    session::Session,
};
use horizon_core::position::BlockPos;
use std::rc::Rc;
use wasm_bindgen::JsValue;
use web_sys::HtmlCanvasElement;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 3],
    uv: [f32; 2],
    tile: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    count: u32,
    translucent_vertices: wgpu::Buffer,
    translucent_count: u32,
    atlas_layout: wgpu::BindGroupLayout,
    atlas_bind: wgpu::BindGroup,
    resources: Option<Rc<Resources>>,
    depth: wgpu::TextureView,
    canvas: HtmlCanvasElement,
}

fn error(message: &str) -> JsValue {
    JsValue::from_str(message)
}

fn depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("Browser terrain depth"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24Plus,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

impl Renderer {
    pub async fn new(canvas: HtmlCanvasElement) -> Result<Self, JsValue> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::BROWSER_WEBGPU;
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|_| error("Could not create a WebGPU canvas."))?;
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface), power_preference: wgpu::PowerPreference::HighPerformance, force_fallback_adapter: false, ..Default::default()
        }).await.map_err(|_| error("WebGPU is unavailable in this browser. The connection menu and server checks still work."))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Horizon browser"),
                ..Default::default()
            })
            .await
            .map_err(|_| error("Could not initialize WebGPU."))?;
        let width = canvas.client_width().max(1) as u32;
        let height = canvas.client_height().max(1) as u32;
        canvas.set_width(width);
        canvas.set_height(height);
        let config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| error("No supported WebGPU canvas format."))?;
        surface.configure(&device, &config);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Block atlas"),
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
        let atlas_bind = upload_atlas(&device, &queue, &atlas_layout, &uniform, &Atlas::fallback());
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout), Some(&atlas_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
        });
        let make_pipeline = |translucent| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Horizon browser terrain"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Uint32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: if translucent { Some(wgpu::BlendState::ALPHA_BLENDING) } else {None},
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: Some(!translucent),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
        };
        let pipeline = make_pipeline(false);
        let translucent_pipeline = make_pipeline(true);
        let vertices = vertex_buffer(&device, &[]);
        let translucent_vertices = vertex_buffer(&device, &[]);
        let depth = depth(&device, width, height);
        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            translucent_pipeline,
            uniform,
            bind,
            vertices,
            count: 0,
            translucent_vertices,
            translucent_count: 0,
            atlas_layout,
            atlas_bind,
            resources: None,
            depth,
            canvas,
        })
    }

    pub fn clear(&mut self) {
        self.count = 0;
        self.translucent_count = 0;
    }

    pub fn set_resources(&mut self, resources: Option<Rc<Resources>>) {
        self.atlas_bind = upload_atlas(
            &self.device,
            &self.queue,
            &self.atlas_layout,
            &self.uniform,
            resources
                .as_ref()
                .map(|r| &r.atlas)
                .unwrap_or(&Atlas::fallback()),
        );
        self.resources = resources;
        self.clear();
    }

    pub fn mesh(&mut self, session: &Session) {
        let mut vertices = vec![];
        let mut transparent: Vec<(f64, [Vertex; 6])> = vec![];
        let center = [
            session.position[0].floor() as i32,
            session.position[1].floor() as i32,
            session.position[2].floor() as i32,
        ];
        // Bound main-thread mesh work until native streaming moves to a worker.
        for (&(cx, cz), sections) in &session.chunks {
            if (cx - center[0].div_euclid(16)).abs() > 1
                || (cz - center[2].div_euclid(16)).abs() > 1
            {
                continue;
            }
            for sy in 0..sections.len() {
                let y0 = session.dimension.min_y + sy as i32 * 16;
                if y0 + 16 < center[1] - 48 || y0 > center[1] + 48 {
                    continue;
                }
                for i in 0..4096 {
                    let p = [cx * 16 + (i & 15), y0 + (i >> 8), cz * 16 + ((i >> 4) & 15)];
                    let state = session.block(p);
                    if self
                        .resources
                        .as_ref()
                        .map_or(state == 0, |r| r.is_air(state))
                    {
                        continue;
                    }
                    if let Some(resources) = &self.resources {
                        for face in resources.faces(state, BlockPos::new(p[0], p[1], p[2])) {
                            if vertices.len() + transparent.len() * 6 + 6 > 1_000_000 {
                                break;
                            }
                            let quad = &face.quad;
                            if let Some(cull) = quad.cull {
                                let normal = MODEL_NORMALS[cull as usize];
                                let neighbor = std::array::from_fn(|a| p[a] + normal[a]);
                                if !resources.visible(
                                    state,
                                    session.block(neighbor),
                                    direction(cull),
                                ) {
                                    continue;
                                }
                            }
                            let shade: f32 = [0.5, 1.0, 0.8, 0.8, 0.6, 0.6]
                                [quad.shade_direction_override.unwrap_or(quad.facing) as usize];
                            let shade = shade.max(f32::from(quad.light_emission) / 15.0);
                            // Biome-aware tint and world lighting are not yet ported.
                            let tint = if quad.tint_index >= 0 {
                                [0.62, 0.8, 0.42]
                            } else {
                                [1.; 3]
                            };
                            let data = [0, 1, 2, 0, 2, 3].map(|v| Vertex {
                                position: std::array::from_fn(|a| {
                                    p[a] as f32 + quad.positions[v][a]
                                }),
                                color: tint.map(|c| c * shade),
                                uv: quad.uv[v],
                                tile: face.tile,
                            });
                            if face.translucent {
                                let distance = (0..3)
                                    .map(|a| {
                                        let middle = quad
                                            .positions
                                            .iter()
                                            .map(|v| f64::from(v[a]))
                                            .sum::<f64>()
                                            / 4.0;
                                        let eye =
                                            session.position[a] + if a == 1 { 1.62 } else { 0.0 };
                                        (f64::from(p[a]) + middle - eye).powi(2)
                                    })
                                    .sum();
                                transparent.push((distance, data));
                            } else {
                                vertices.extend(data);
                            }
                        }
                    } else {
                        let color = state_color(state);
                        for (face, normal) in NORMALS.iter().enumerate() {
                            let neighbour = std::array::from_fn(|axis| p[axis] + normal[axis]);
                            if session.block(neighbour) != 0 {
                                continue;
                            }
                            let shade = [0.72, 0.72, 1.0, 0.5, 0.85, 0.85][face];
                            for v in [0, 1, 2, 0, 2, 3] {
                                vertices.push(Vertex {
                                    position: std::array::from_fn(|a| {
                                        p[a] as f32 + FACES[face][v][a]
                                    }),
                                    color: color.map(|c| c * shade),
                                    uv: [[0., 1.], [0., 0.], [1., 0.], [1., 1.]][v],
                                    tile: 0,
                                });
                            }
                        }
                    }
                    if vertices.len() + transparent.len() * 6 >= 999_996 {
                        break;
                    }
                }
                if vertices.len() + transparent.len() * 6 >= 999_996 {
                    break;
                }
            }
            if vertices.len() + transparent.len() * 6 >= 999_996 {
                break;
            }
        }
        self.count = vertices.len() as u32;
        self.vertices = vertex_buffer(&self.device, &vertices);
        transparent.sort_by(|a, b| b.0.total_cmp(&a.0));
        let transparent: Vec<Vertex> = transparent.into_iter().flat_map(|(_, face)| face).collect();
        self.translucent_count = transparent.len() as u32;
        self.translucent_vertices = vertex_buffer(&self.device, &transparent);
    }

    pub fn render(&mut self, position: [f64; 3], rotation: (f32, f32)) {
        let width = self.canvas.client_width().clamp(1, 2560) as u32;
        let height = self.canvas.client_height().clamp(1, 1440) as u32;
        if width != self.config.width || height != self.config.height {
            self.canvas.set_width(width);
            self.canvas.set_height(height);
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.depth = depth(&self.device, width, height);
        }
        let yaw = rotation.0.to_radians();
        let pitch = rotation.1.to_radians();
        let eye = glam::Vec3::new(
            position[0] as f32,
            position[1] as f32 + 1.62,
            position[2] as f32,
        );
        let direction = glam::Vec3::new(
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        let matrix = glam::Mat4::perspective_rh(
            70f32.to_radians(),
            width as f32 / height as f32,
            0.05,
            512.0,
        ) * glam::Mat4::look_at_rh(eye, eye + direction, glam::Vec3::Y);
        self.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::cast_slice(&matrix.to_cols_array()),
        );
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.16,
                            g: 0.23,
                            b: 0.34,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_bind_group(1, &self.atlas_bind, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.draw(0..self.count, 0..1);
            pass.set_pipeline(&self.translucent_pipeline);
            pass.set_vertex_buffer(0, self.translucent_vertices.slice(..));
            pass.draw(0..self.translucent_count, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}

fn vertex_buffer(device: &wgpu::Device, vertices: &[Vertex]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Browser block mesh"),
        contents: if vertices.is_empty() {
            &[0; 36]
        } else {
            bytemuck::cast_slice(vertices)
        },
        usage: wgpu::BufferUsages::VERTEX,
    })
}

fn upload_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    atlas: &Atlas,
) -> wgpu::BindGroup {
    let size = wgpu::Extent3d {
        width: atlas.side,
        height: atlas.side,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Local block textures"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas.side * 4),
            rows_per_image: Some(atlas.side),
        },
        size,
    );
    let view = texture.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    queue.write_buffer(
        uniform,
        64,
        bytemuck::cast_slice(&[atlas.columns as f32, 16.0, atlas.side as f32, 0.0]),
    );
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Block atlas"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    })
}

const MODEL_NORMALS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

fn state_color(state: i32) -> [f32; 3] {
    // Explicit placeholder colours, not a claim about vanilla block appearance.
    let hash = (state as u32).wrapping_mul(2654435761);
    [
        0.3 + (hash & 255) as f32 / 640.0,
        0.35 + ((hash >> 8) & 255) as f32 / 640.0,
        0.3 + ((hash >> 16) & 255) as f32 / 640.0,
    ]
}

const NORMALS: [[i32; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, -1],
    [0, 0, 1],
];
const FACES: [[[f32; 3]; 4]; 6] = [
    [[0., 0., 0.], [0., 1., 0.], [0., 1., 1.], [0., 0., 1.]],
    [[1., 0., 1.], [1., 1., 1.], [1., 1., 0.], [1., 0., 0.]],
    [[0., 1., 1.], [0., 1., 0.], [1., 1., 0.], [1., 1., 1.]],
    [[0., 0., 0.], [0., 0., 1.], [1., 0., 1.], [1., 0., 0.]],
    [[1., 0., 0.], [1., 1., 0.], [0., 1., 0.], [0., 0., 0.]],
    [[0., 0., 1.], [0., 1., 1.], [1., 1., 1.], [1., 0., 1.]],
];
