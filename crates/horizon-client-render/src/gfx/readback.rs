//! Screenshots of a swapchain that cannot be copied from. OpenGL's surfaces
//! offer `RENDER_ATTACHMENT` only, so a frame whose pixels are read back
//! (the screenshot key, a world's icon) is drawn into a texture that can be,
//! then drawn onto the swapchain image as a fullscreen triangle.
//!
//! The same frame texture carries Android's host-launcher IME inset: while the
//! host's keyboard covers the bottom of the surface, the finished frame is drawn
//! moved up by just enough to keep the focused text field above it, so Minecraft's
//! render size and GUI scale never change.

/// The texture a frame to read back is drawn into, and the pass that puts it
/// on the swapchain afterwards.
#[derive(Default)]
pub struct Readback {
    texture: Option<wgpu::Texture>,
    blitter: Option<(wgpu::TextureFormat, wgpu::util::TextureBlitter)>,
    /// The pas that lifts the frame by the host's IME inset (built at its first use).
    shift: Option<Shifted>,
    /// A frame drew into `texture` and still has to reach the swapchain.
    drawn: bool,
}

/// On Android, the host launcher's bottom inset in window pixels; 0 elsewhere.
fn ime_inset() -> u32 {
    #[cfg(target_os = "android")]
    {
        super::android_ime_viewport_bottom_inset()
    }
    #[cfg(not(target_os = "android"))]
    {
        0
    }
}

const SHIFT_SHADER: &str = r#"
@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

struct PresentUniform {
    // Fraction of the presented height to move the source upward.
    shift_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};
@group(0) @binding(2) var<uniform> present: PresentUniform;

struct PostOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex fn vs_present(@builtin(vertex_index) i: u32) -> PostOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: PostOut;
    out.clip = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(p.x, 1.0 - p.y);
    return out;
}

@fragment fn fs_present(in: PostOut) -> @location(0) vec4<f32> {
    // The frame moves up: the presented row samples one row further down, and the
    // band the keyboard covers at the bottom stays black.
    let uv = vec2<f32>(in.uv.x, in.uv.y + present.shift_y);
    if (uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    return textureSampleLevel(frame_tex, frame_sampler, uv, 0.0);
}
"#;

/// The pipeline that copies the frame texture to the swapchain, moved up by the inset.
struct Shifted {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
}

impl Shifted {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Android frame presentation"),
            source: wgpu::ShaderSource::Wgsl(SHIFT_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Android frame presentation bindings"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Android frame presentation"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Android frame presentation"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_present"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_present"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Android frame presentation"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Android frame presentation uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            format,
            layout,
            sampler,
            pipeline,
            uniform,
        }
    }
}

impl Readback {
    /// The texture this frame draws into instead of the swapchain image
    /// `surface`: `None` unless the frame is read back (`screenshot`) and the
    /// image cannot be copied from, or the host's IME inset has to lift it.
    pub fn target(
        &mut self,
        device: &wgpu::Device,
        surface: &wgpu::Texture,
        screenshot: bool,
    ) -> Option<wgpu::Texture> {
        let shift = ime_inset() > 0;
        #[cfg(target_os = "android")]
        if !shift {
            // 键盘收起:呈现帧不再平移,宿主坐标也不必换算。
            super::set_android_frame_shift(0);
        }
        if !shift && (!screenshot || surface.usage().contains(wgpu::TextureUsages::COPY_SRC)) {
            return None;
        }
        let format = surface.format().remove_srgb_suffix();
        if self
            .texture
            .as_ref()
            .is_none_or(|t| t.size() != surface.size() || t.format() != format)
        {
            self.texture = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Horizon screenshot frame"),
                size: surface.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            }));
        }
        self.drawn = true;
        self.texture.clone()
    }

    /// Draws the frame `target` gave onto the swapchain image `surface`; nothing
    /// when this frame drew straight into it.
    pub fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        surface: &wgpu::Texture,
    ) {
        if !std::mem::take(&mut self.drawn) {
            return;
        }
        let Some(texture) = &self.texture else {
            return;
        };
        // The frame's passes write the swapchain through its non-sRGB view
        // (`view_formats`), so the copy goes there unconverted too.
        let format = surface.format().remove_srgb_suffix();
        let target = surface.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format),
            ..Default::default()
        });
        let source = texture.create_view(&Default::default());
        let inset = ime_inset();
        if inset > 0 {
            let shifted = match &self.shift {
                Some(shift) if shift.format == format => shift,
                _ => {
                    let shift = Shifted::new(device, format);
                    self.shift.insert(shift)
                }
            };
            let height = surface.height().max(1);
            #[cfg(target_os = "android")]
            let shift_px = {
                let shift = super::android_text_field()
                    .map(|field| {
                        // 让焦点输入框整体落在键盘上沿之上,再留一点边距。
                        let keyboard_top = height.saturating_sub(inset) as f32;
                        let margin = height as f32 * 0.02;
                        (field[3] + margin - keyboard_top).max(0.0)
                    })
                    .unwrap_or(0.0)
                    .min(height.saturating_sub(1) as f32);
                let shift_px = shift.round() as u32;
                super::set_android_frame_shift(shift_px);
                shift_px
            };
            #[cfg(not(target_os = "android"))]
            let shift_px = 0u32;
            let ratio = shift_px as f32 / height as f32;
            static LAST: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(u64::MAX);
            let key = (u64::from(inset) << 32) | u64::from(shift_px);
            if LAST.swap(key, std::sync::atomic::Ordering::Relaxed) != key {
                log::info!(
                    "Android frame presentation: keyboard {inset} px of {height}, frame moved up {shift_px} px"
                );
            }
            queue.write_buffer(
                &shifted.uniform,
                0,
                bytemuck::cast_slice(&[ratio, 0.0, 0.0, 0.0]),
            );
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Android frame presentation"),
                layout: &shifted.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&shifted.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: shifted.uniform.as_entire_binding(),
                    },
                ],
            });
            let attachments = [Some(wgpu::RenderPassColorAttachment {
                view: &target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Android frame presentation"),
                color_attachments: &attachments,
                ..Default::default()
            });
            pass.set_pipeline(&shifted.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
            return;
        }
        if self.blitter.as_ref().is_none_or(|b| b.0 != format) {
            self.blitter = Some((format, wgpu::util::TextureBlitter::new(device, format)));
        }
        if let Some((_, blitter)) = &self.blitter {
            blitter.copy(device, encoder, &source, &target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = crate::gfx::test_gpu::instance();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        pollster::block_on(adapter.request_device(&Default::default())).ok()
    }

    fn texture(device: &wgpu::Device, usage: wgpu::TextureUsages) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 70,
                height: 3,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage,
            view_formats: &[],
        })
    }

    fn clear(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, color: wgpu::Color) {
        let view = texture.create_view(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
    }

    fn read(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        buffer.get_mapped_range(..).unwrap().to_vec()
    }

    /// The last pixel of the last row of a 70x3 frame read back by `copy_frame`.
    fn last_pixel(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
        let pixels = read(device, buffer);
        let row = 70usize * 4;
        let last = row.next_multiple_of(256) * 2 + row - 4;
        pixels[last..last + 4].to_vec()
    }

    /// The renderer's screenshot path on a surface like OpenGL's (#3510): the
    /// frame is drawn into `target`, read back with `copy_frame` and drawn on
    /// the swapchain, with no validation error (the copy used to need the
    /// swapchain's missing `COPY_SRC`), and the swapchain shows the frame.
    #[test]
    fn screenshot_of_a_surface_without_copy_src() {
        let Some((device, queue)) = device() else {
            return;
        };
        // Sampled only so the test can look at what reached it.
        let surface = texture(
            &device,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let check = texture(
            &device,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let blitter = wgpu::util::TextureBlitter::new(&device, wgpu::TextureFormat::Bgra8Unorm);
        let mut readback = Readback::default();
        let colors = [
            (1.0, [0, 0, 255, 255]),
            // A second screenshot reuses the texture.
            (0.0, [255, 0, 0, 255]),
        ];
        for (r, bgra) in colors {
            let color = wgpu::Color {
                r,
                g: 0.0,
                b: 1.0 - r,
                a: 1.0,
            };
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut encoder = device.create_command_encoder(&Default::default());
            let frame = readback
                .target(&device, &surface, true)
                .unwrap_or_else(|| surface.clone());
            clear(&mut encoder, &frame, color);
            let buffer = crate::gfx::copy_frame(&device, &mut encoder, &frame, [70, 3]);
            readback.present(&device, &queue, &mut encoder, &surface);
            queue.submit([encoder.finish()]);
            if let Some(error) = pollster::block_on(scope.pop()) {
                panic!("{error}");
            }
            assert_eq!(last_pixel(&device, &buffer), bgra);
            let mut encoder = device.create_command_encoder(&Default::default());
            let view = |t: &wgpu::Texture| t.create_view(&Default::default());
            blitter.copy(&device, &mut encoder, &view(&surface), &view(&check));
            let buffer = crate::gfx::copy_frame(&device, &mut encoder, &check, [70, 3]);
            queue.submit([encoder.finish()]);
            assert_eq!(last_pixel(&device, &buffer), bgra);
        }
        assert!(!readback.drawn);
    }

    /// A surface that can be copied from, or a frame nobody reads back,
    /// draws straight into the swapchain image.
    #[test]
    fn frames_draw_straight_to_a_readable_surface() {
        let Some((device, _)) = device() else {
            return;
        };
        let mut readback = Readback::default();
        let readable = texture(
            &device,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        assert!(readback.target(&device, &readable, true).is_none());
        let surface = texture(&device, wgpu::TextureUsages::RENDER_ATTACHMENT);
        assert!(readback.target(&device, &surface, false).is_none());
        assert!(readback.texture.is_none() && !readback.drawn);
    }

    /// The frame moves up above the keyboard: the rows it leaves at the bottom stay black.
    #[test]
    fn frame_moves_up_above_the_keyboard() {
        let Some((device, queue)) = device() else {
            return;
        };
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let size = wgpu::Extent3d {
            width: 4,
            height: 8,
            depth_or_array_layers: 1,
        };
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut pixels = Vec::new();
        for row in 0u8..8 {
            let value = 40 + row * 24;
            for _ in 0..4 {
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16),
                rows_per_image: Some(8),
            },
            size,
        );
        let frame = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let scaled = Shifted::new(&device, format);
        // A keyboard covering half the surface: the frame keeps the top half.
        queue.write_buffer(
            &scaled.uniform,
            0,
            bytemuck::cast_slice(&[0.5f32, 0.0, 0.0, 0.0]),
        );
        let source_view = source.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &scaled.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&scaled.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: scaled.uniform.as_entire_binding(),
                },
            ],
        });
        let frame_view = frame.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &frame_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&scaled.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        let buffer = crate::gfx::copy_frame(&device, &mut encoder, &frame, [4, 8]);
        queue.submit([encoder.finish()]);
        let presented = read(&device, &buffer);
        let stride = (4usize * 4).next_multiple_of(256);
        let red = |row: usize| presented[stride * row + 2];
        assert!(red(0) > 0, "the frame's first row is on the surface");
        for row in 4..8 {
            assert_eq!(red(row), 0, "row {row} is under the keyboard");
        }
    }
}
