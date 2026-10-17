//! wgpu 執行器：每個融合 kernel 一次 compute dispatch + 背景求值執行緒。
//!
//! - pipeline 以子圖結構 hash 快取，改參數不重編。
//! - 參數集中寫入單一 storage buffer，各 kernel 以對齊後的 offset 綁定。
//! - kernel 輸出依內容 hash 存進 LRU 快取（有記憶體上限），跨影格共用：
//!   與時間無關的分支整段動畫只算一次。被淘汰的 texture 回收到依尺寸分桶的 pool。

use pixelnode_compiler::{MAX_KERNEL_TEXTURES, Plan};
use std::collections::HashMap;

pub mod worker;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// texture pool 閒置上限（位元組），超過就直接釋放。
const POOL_BUDGET: u64 = 256 << 20;
/// 輸出快取預設上限（位元組）。
pub const DEFAULT_CACHE_BUDGET: u64 = 512 << 20;

pub mod export;

struct CacheEntry {
    tex: wgpu::Texture,
    last_used: u64,
}

#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// RGBA8，未預乘 alpha，逐列緊密排列。
    pub rgba: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub kernels: usize,
    pub dispatched: usize,
    pub cached: usize,
    pub compiled: usize,
}

/// 依（寬、高）分桶重用 texture。格式固定 RGBA8，日後加 RGBA16F 再擴充 key。
#[derive(Default)]
struct TexturePool {
    free: HashMap<(u32, u32), Vec<wgpu::Texture>>,
    idle_bytes: u64,
}

impl TexturePool {
    fn acquire(&mut self, device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        match self.free.get_mut(&(w, h)).and_then(Vec::pop) {
            Some(t) => {
                self.idle_bytes -= tex_bytes(w, h);
                t
            }
            None => create_texture(device, w, h),
        }
    }

    fn release(&mut self, tex: wgpu::Texture) {
        let (w, h) = (tex.width(), tex.height());
        if self.idle_bytes + tex_bytes(w, h) > POOL_BUDGET {
            return;
        }
        self.idle_bytes += tex_bytes(w, h);
        self.free.entry((w, h)).or_default().push(tex);
    }
}

fn tex_bytes(w: u32, h: u32) -> u64 {
    w as u64 * h as u64 * 4
}

pub struct Runtime {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<u64, wgpu::ComputePipeline>,
    dummy: wgpu::TextureView,
    info: wgpu::Buffer,
    params: wgpu::Buffer,
    param_align: u64,
    readback: Option<wgpu::Buffer>,
    pool: TexturePool,
    /// kernel 內容 hash → 輸出，LRU。
    cache: HashMap<u64, CacheEntry>,
    cache_bytes: u64,
    pub cache_budget: u64,
    tick: u64,
    pub stats: Stats,
    pub adapter_name: String,
    /// 預覽用 texture：只由 copy 寫入，不進 pool，UI 可安全持有。
    display: Option<wgpu::Texture>,
}

impl Runtime {
    pub fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let request = |fallback| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: fallback,
                ..Default::default()
            }))
        };
        // 沒有實體 GPU 時（例如 CI）退回軟體 adapter。
        let adapter = request(false).or_else(|_| request(true)).map_err(|e| format!("找不到 GPU adapter：{e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("pixelnode-runtime"),
            ..Default::default()
        }))
        .map_err(|e| format!("建立 GPU device 失敗：{e}"))?;
        Ok(Self::with_device(device, queue, adapter.get_info().name))
    }

    /// 使用既有 device（例如與 eframe 共用，預覽不必讀回 CPU）。
    pub fn with_device(device: wgpu::Device, queue: wgpu::Queue, adapter_name: String) -> Self {

        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];
        for t in 0..MAX_KERNEL_TEXTURES as u32 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: t + 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: MAX_KERNEL_TEXTURES as u32 + 2,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fused-kernel"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fused-kernel"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });

        let dummy = create_texture(&device, 1, 1).create_view(&Default::default());
        let info = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("info"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let param_align = device.limits().min_storage_buffer_offset_alignment as u64;
        let params = create_param_buffer(&device, 64 << 10);

        Self {
            adapter_name,
            device,
            queue,
            layout,
            pipeline_layout,
            pipelines: HashMap::new(),
            dummy,
            info,
            params,
            param_align,
            readback: None,
            pool: TexturePool::default(),
            cache: HashMap::new(),
            cache_bytes: 0,
            cache_budget: DEFAULT_CACHE_BUDGET,
            tick: 0,
            stats: Stats::default(),
            display: None,
        }
    }

    /// 清空輸出快取（benchmark 冷啟動用；pipeline 快取保留）。
    pub fn clear_cache(&mut self) {
        for (_, e) in self.cache.drain() {
            self.pool.release(e.tex);
        }
        self.cache_bytes = 0;
    }

    /// 執行並把 Output 讀回 CPU。
    pub fn execute(&mut self, plan: &Plan, frame: u32) -> Image {
        let (encoder, out) = self.encode(plan, frame);
        self.read_back(encoder, &out)
    }

    /// 執行並把 Output 複製到預覽 texture（GPU 內複製）。
    /// 回傳的 bool 表示 texture 是新建的（尺寸改變），UI 需重新註冊。
    pub fn execute_to_display(&mut self, plan: &Plan, frame: u32) -> (wgpu::Texture, bool) {
        let (mut encoder, out) = self.encode(plan, frame);
        let (w, h) = (out.width(), out.height());
        let fresh = self.display.as_ref().is_none_or(|d| d.width() != w || d.height() != h);
        if fresh {
            self.display = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("display"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
        }
        let display = self.display.clone().unwrap();
        encoder.copy_texture_to_texture(
            out.as_image_copy(),
            display.as_image_copy(),
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU poll failed");
        (display, fresh)
    }

    /// 執行並等待 GPU 完成，不讀回（benchmark 用）。
    pub fn run(&mut self, plan: &Plan, frame: u32) {
        let (encoder, _) = self.encode(plan, frame);
        if self.stats.dispatched == 0 {
            return;
        }
        self.queue.submit([encoder.finish()]);
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU poll failed");
    }

    fn encode(&mut self, plan: &Plan, frame: u32) -> (wgpu::CommandEncoder, wgpu::Texture) {
        let [w, h] = plan.canvas;
        self.stats = Stats { kernels: plan.kernels.len(), ..Default::default() };
        self.queue.write_buffer(&self.info, 0, bytemuck::cast_slice(&[w, h, frame, 0]));

        // 參數打包進單一 buffer。
        let mut offsets = Vec::with_capacity(plan.kernels.len());
        let mut bytes: Vec<u8> = Vec::new();
        for k in &plan.kernels {
            let off = (bytes.len() as u64).next_multiple_of(self.param_align);
            bytes.resize(off as usize, 0);
            offsets.push(off);
            if k.params.is_empty() {
                bytes.extend_from_slice(&[0u8; 16]);
            } else {
                bytes.extend_from_slice(bytemuck::cast_slice(&k.params));
            }
        }
        if bytes.len() as u64 > self.params.size() {
            self.params = create_param_buffer(&self.device, (bytes.len() as u64).next_power_of_two());
        }
        self.queue.write_buffer(&self.params, 0, &bytes);

        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut outputs: Vec<wgpu::Texture> = Vec::with_capacity(plan.kernels.len());
        self.tick += 1;
        let tick = self.tick;
        let mut binds = Vec::with_capacity(plan.kernels.len());

        for (ki, kernel) in plan.kernels.iter().enumerate() {
            if let Some(e) = self.cache.get_mut(&kernel.hash) {
                self.stats.cached += 1;
                e.last_used = tick;
                outputs.push(e.tex.clone());
                continue;
            }
            self.stats.dispatched += 1;

            if !self.pipelines.contains_key(&kernel.shader_key) {
                self.stats.compiled += 1;
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("fused-kernel"),
                    source: wgpu::ShaderSource::Wgsl(plan.kernel_wgsl(ki).into()),
                });
                let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("fused-kernel"),
                    layout: Some(&self.pipeline_layout),
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
                self.pipelines.insert(kernel.shader_key, pipeline);
            }

            let out = self.pool.acquire(&self.device, w, h);
            let views: Vec<wgpu::TextureView> = (0..MAX_KERNEL_TEXTURES)
                .map(|t| match kernel.textures.get(t) {
                    Some(&src) => outputs[src].create_view(&Default::default()),
                    None => self.dummy.clone(),
                })
                .collect();
            let out_view = out.create_view(&Default::default());
            let param_size = (kernel.params.len().max(1) * 16) as u64;
            let mut entries = vec![
                wgpu::BindGroupEntry { binding: 0, resource: self.info.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.params,
                        offset: offsets[ki],
                        size: wgpu::BufferSize::new(param_size),
                    }),
                },
            ];
            for (t, v) in views.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry { binding: t as u32 + 2, resource: wgpu::BindingResource::TextureView(v) });
            }
            entries.push(wgpu::BindGroupEntry {
                binding: MAX_KERNEL_TEXTURES as u32 + 2,
                resource: wgpu::BindingResource::TextureView(&out_view),
            });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &entries,
            });
            binds.push((kernel.shader_key, bind));
            self.cache_bytes += tex_bytes(w, h);
            self.cache.insert(kernel.hash, CacheEntry { tex: out.clone(), last_used: tick });
            outputs.push(out);
        }

        // 全部 dispatch 放進同一個 compute pass；wgpu 會在 kernel 間插入必要的 barrier。
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            for (key, bind) in &binds {
                pass.set_pipeline(&self.pipelines[key]);
                pass.set_bind_group(0, bind, &[]);
                pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
            }
        }

        self.evict(tick);
        (encoder, outputs.pop().expect("plan has an output kernel"))
    }

    /// 超過上限時淘汰最久未用、且本輪沒用到的輸出。
    fn evict(&mut self, tick: u64) {
        if self.cache_bytes <= self.cache_budget {
            return;
        }
        let mut old: Vec<(u64, u64)> =
            self.cache.iter().filter(|(_, e)| e.last_used < tick).map(|(k, e)| (e.last_used, *k)).collect();
        old.sort_unstable();
        for (_, k) in old {
            if self.cache_bytes <= self.cache_budget {
                break;
            }
            let e = self.cache.remove(&k).unwrap();
            self.cache_bytes -= tex_bytes(e.tex.width(), e.tex.height());
            self.pool.release(e.tex);
        }
    }

    pub fn cache_bytes(&self) -> u64 {
        self.cache_bytes
    }

    fn read_back(&mut self, mut encoder: wgpu::CommandEncoder, out: &wgpu::Texture) -> Image {
        let (w, h) = (out.width(), out.height());
        let padded = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = (padded * h) as u64;
        if self.readback.as_ref().is_none_or(|b| b.size() < size) {
            self.readback = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let readback = self.readback.as_ref().unwrap();
        encoder.copy_texture_to_buffer(
            out.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..size);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("readback map failed"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU poll failed");
        let data = slice.get_mapped_range().expect("readback range");
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for row in data.chunks(padded as usize) {
            rgba.extend_from_slice(&row[..(w * 4) as usize]);
        }
        drop(data);
        readback.unmap();
        Image { width: w, height: h, rgba }
    }
}

fn create_param_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("params"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixelnode_compiler::{CompileOptions, compile, compile_with};
    use pixelnode_graph::{Graph, Link, Node, NodeId, Value};

    fn n(id: u64, kind: &str) -> Node {
        Node { id: NodeId(id), kind: kind.into(), params: Default::default(), pos: [0.0; 2], anim: Default::default() }
    }
    fn link(a: u64, b: u64, port: usize) -> Link {
        Link { from_node: NodeId(a), from_port: 0, to_node: NodeId(b), to_port: port }
    }

    fn red_to_green() -> Graph {
        let mut solid = n(1, "solid");
        solid.params.insert("color".into(), Value::Color([1.0, 0.0, 0.0, 1.0]));
        let mut adj = n(2, "color_adjust");
        adj.params.insert("hue".into(), Value::Float(1.0 / 3.0));
        Graph {
            canvas: [17, 9],
            nodes: vec![solid, adj, n(3, "output")],
            links: vec![link(1, 2, 0), link(2, 3, 0)],
            ..Default::default()
        }
    }

    /// 較複雜的圖：共用節點（融合邊界）+ 兩個 noise + 多種 blend。
    fn mixed() -> Graph {
        let mut blend = n(5, "blend");
        blend.params.insert("mode".into(), Value::Enum(4));
        let mut noise2 = n(2, "noise");
        noise2.params.insert("type".into(), Value::Enum(2));
        Graph {
            canvas: [33, 20],
            nodes: vec![n(1, "noise"), noise2, n(3, "color_adjust"), n(4, "blend"), blend, n(6, "output")],
            links: vec![
                link(1, 3, 0),
                link(1, 4, 0),
                link(3, 4, 1),
                link(4, 5, 0),
                link(2, 5, 1),
                link(5, 6, 0),
            ],
            ..Default::default()
        }
    }

    /// 需要 GPU（或軟體 adapter）；都沒有時略過。
    #[test]
    fn executes_and_caches() {
        let Ok(mut rt) = Runtime::new() else { return };
        let plan = compile(&red_to_green()).unwrap();
        let img = rt.execute(&plan, 0);
        assert_eq!((img.width, img.height, img.rgba.len()), (17, 9, 17 * 9 * 4));
        let px = &img.rgba[..4];
        assert!(px[1] > 240 && px[0] < 15 && px[2] < 15, "{px:?}");
        assert_eq!((rt.stats.kernels, rt.stats.dispatched), (1, 1));

        rt.execute(&plan, 0);
        assert_eq!((rt.stats.dispatched, rt.stats.cached), (0, 1));
    }

    #[test]
    fn fused_matches_unfused() {
        let Ok(mut rt) = Runtime::new() else { return };
        let g = mixed();
        let fused = compile(&g).unwrap();
        let unfused = compile_with(&g, CompileOptions { fuse: false, ..Default::default() }).unwrap();
        assert!(fused.kernels.len() < unfused.kernels.len());
        let a = rt.execute(&fused, 0);
        rt.clear_cache();
        let b = rt.execute(&unfused, 0);
        // 融合後中間值不經 RGBA8 量化，允許 ±2 的誤差。
        let max_diff = a.rgba.iter().zip(&b.rgba).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(max_diff <= 2, "max diff {max_diff}");
    }

    #[test]
    fn lru_keeps_frames_within_budget() {
        let Ok(mut rt) = Runtime::new() else { return };
        let mut g = red_to_green();
        let one = tex_bytes(17, 9);
        rt.cache_budget = one * 3;
        for i in 0..6 {
            g.nodes[1].params.insert("hue".into(), Value::Float(i as f32 * 0.1));
            rt.execute(&compile(&g).unwrap(), 0);
        }
        assert!(rt.cache_bytes() <= one * 3);
        // 最近一次仍在快取中
        rt.execute(&compile(&g).unwrap(), 0);
        assert_eq!(rt.stats.cached, 1);
        // 最早的已被淘汰
        g.nodes[1].params.insert("hue".into(), Value::Float(0.0));
        rt.execute(&compile(&g).unwrap(), 0);
        assert_eq!(rt.stats.dispatched, 1);
    }

    #[test]
    fn param_change_reuses_pipeline() {
        let Ok(mut rt) = Runtime::new() else { return };
        let mut g = red_to_green();
        rt.execute(&compile(&g).unwrap(), 0);
        assert_eq!(rt.stats.compiled, 1);
        g.nodes[1].params.insert("hue".into(), Value::Float(0.1));
        rt.execute(&compile(&g).unwrap(), 0);
        assert_eq!((rt.stats.compiled, rt.stats.dispatched), (0, 1));
    }
}
