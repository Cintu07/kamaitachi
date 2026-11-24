//! Pixels: Direct3D 11, one shader, one instanced quad per thing drawn. Rounded boxes
//! and borders are a distance field in the shader; glyphs come from an R8 atlas that
//! DirectWrite rasterizes into once per glyph and size; images are their own textures,
//! decoded at the size they are shown.

use std::collections::HashMap;
use windows::core::{Interface, Result, PCSTR};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

const SHADER: &str = r#"
cbuffer C : register(b0) { float4 vp; };
struct Q { float4 rect : RECT; float4 color : COLOR; float4 uv : UV; float4 prm : PRM; float4 clip : CLIP; };
struct V { float4 pos : SV_Position; float4 color : COLOR; float2 uv : UV; float2 local : LOC; float2 half_ : HALF; float4 prm : PRM; float4 clip : CLIP; };
V vs(uint vid : SV_VertexID, Q q) {
    float2 c = float2(vid & 1, vid >> 1);
    float2 p = q.rect.xy + c * q.rect.zw;
    V o;
    o.pos = float4(p / vp.xy * float2(2, -2) + float2(-1, 1), 0, 1);
    o.color = q.color;
    o.uv = lerp(q.uv.xy, q.uv.zw, c);
    o.local = (c - 0.5) * q.rect.zw;
    o.half_ = q.rect.zw * 0.5;
    o.prm = q.prm;
    o.clip = q.clip;
    return o;
}
Texture2D<float> atlas : register(t0);
Texture2D img : register(t1);
SamplerState lin : register(s0);
float4 ps(V i) : SV_Target {
    if (i.pos.x < i.clip.x || i.pos.y < i.clip.y || i.pos.x >= i.clip.z || i.pos.y >= i.clip.w) discard;
    float kind = i.prm.z;
    if (kind == 1) {
        float a = atlas.Load(int3(i.uv, 0));
        // a little contrast, since coverage is blended in gamma space
        a = saturate(pow(a, 0.82) * 1.06);
        return i.color * a;
    }
    float r = i.prm.x;
    float2 d = abs(i.local) - i.half_ + r;
    float dist = length(max(d, 0)) + min(max(d.x, d.y), 0) - r;
    float cov = saturate(0.5 - dist);
    if (kind == 3) cov *= saturate(dist + i.prm.y + 0.5);
    if (kind == 2) return img.Sample(lin, i.uv) * (i.color.a * cov);
    return i.color * cov;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Quad {
    pub rect: [f32; 4],
    pub color: [f32; 4],
    pub uv: [f32; 4],
    pub prm: [f32; 4],
    pub clip: [f32; 4],
}

#[derive(Clone, Copy)]
struct Slot {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    left: i16,
    top: i16,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct GKey {
    face: u16,
    size: u32,
    id: u16,
    sub: u8,
}

const ATLAS: u32 = 2048;

#[derive(Clone)]
pub struct Image {
    pub srv: ID3D11ShaderResourceView,
    pub w: u32,
    pub h: u32,
}

/// A clip rectangle in device pixels: x0, y0, x1, y1.
pub type Clip = [f32; 4];
pub const NO_CLIP: Clip = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

pub struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    cbuf: ID3D11Buffer,
    inst: Option<ID3D11Buffer>,
    inst_cap: usize,
    blend: ID3D11BlendState,
    sampler: ID3D11SamplerState,
    atlas: ID3D11Texture2D,
    atlas_srv: ID3D11ShaderResourceView,
    white: Option<ID3D11ShaderResourceView>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    glyphs: HashMap<GKey, Option<Slot>>,
    dw: IDWriteFactory2,
    pub w: u32,
    pub h: u32,
    quads: Vec<Quad>,
    /// (first quad, image) where the bound image changes
    batches: Vec<(usize, Option<ID3D11ShaderResourceView>)>,
    clear: [f32; 4],
    pub adapter: String,
    /// bytes of image textures alive, for the memory readout
    pub texture_bytes: usize,
}

/// Premultiplied float color.
pub fn rgba(c: [u8; 4], alpha: f32) -> [f32; 4] {
    let a = c[3] as f32 / 255.0 * alpha;
    [c[0] as f32 / 255.0 * a, c[1] as f32 / 255.0 * a, c[2] as f32 / 255.0 * a, a]
}

fn compile(src: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let mut code = None;
    let mut err = None;
    let e = std::ffi::CString::new(entry).unwrap_or_default();
    let t = std::ffi::CString::new(target).unwrap_or_default();
    let r = unsafe {
        D3DCompile(src.as_ptr() as *const _, src.len(), PCSTR::null(), None, None, PCSTR(e.as_ptr() as *const u8), PCSTR(t.as_ptr() as *const u8), 0, 0, &mut code, Some(&mut err))
    };
    if let Err(x) = r {
        if let Some(err) = err {
            let msg = unsafe { std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize()) };
            eprintln!("shader: {}", String::from_utf8_lossy(msg));
        }
        return Err(x);
    }
    code.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

fn blob(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

impl Gpu {
    /// `discrete`: prefer the high-performance gpu (its own memory) over the integrated one.
    pub fn new(hwnd: HWND, discrete: bool) -> Result<Gpu> {
        unsafe {
            let factory: IDXGIFactory6 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            if std::env::var("HAYATE_MEMLOG").is_ok() { eprintln!("  gpu dxgi {:.1}", crate::sys::process_memory().private_bytes as f64 / 1048576.0); }
            let pref = if discrete { DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE } else { DXGI_GPU_PREFERENCE_MINIMUM_POWER };
            let adapter: IDXGIAdapter1 = factory.EnumAdapterByGpuPreference(0, pref)?;
            let desc = adapter.GetDesc1()?;
            let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                if std::env::var("HAYATE_ST").is_ok() { D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED } else { D3D11_CREATE_DEVICE_BGRA_SUPPORT },
                Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
            let device: ID3D11Device = device.expect("device");
            if std::env::var("HAYATE_MEMLOG").is_ok() { eprintln!("  gpu device {:.1}", crate::sys::process_memory().private_bytes as f64 / 1048576.0); }
            let ctx = ctx.expect("context");
            let sd = DXGI_SWAP_CHAIN_DESC1 {
                Width: 0,
                Height: 0,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: if std::env::var("HAYATE_SEQ").is_ok() { DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL } else { DXGI_SWAP_EFFECT_FLIP_DISCARD },
                Scaling: if std::env::var("HAYATE_STRETCH").is_ok() { DXGI_SCALING_STRETCH } else { DXGI_SCALING_NONE },
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let f2: IDXGIFactory2 = factory.cast()?;
            let swap = f2.CreateSwapChainForHwnd(&device, hwnd, &sd, None, None)?;
            let _ = f2.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            if std::env::var("HAYATE_MEMLOG").is_ok() { eprintln!("  gpu swapchain {:.1}", crate::sys::process_memory().private_bytes as f64 / 1048576.0); }
            let vsb = compile(SHADER, "vs", "vs_4_0")?;
            let psb = compile(SHADER, "ps", "ps_4_0")?;
            if std::env::var("HAYATE_MEMLOG").is_ok() { eprintln!("  gpu compiled {:.1}", crate::sys::process_memory().private_bytes as f64 / 1048576.0); }
            let mut vs = None;
            device.CreateVertexShader(blob(&vsb), None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(blob(&psb), None, Some(&mut ps))?;
            let names: [&[u8]; 5] = [b"RECT\0", b"COLOR\0", b"UV\0", b"PRM\0", b"CLIP\0"];
            let elems: Vec<D3D11_INPUT_ELEMENT_DESC> = names
                .iter()
                .enumerate()
                .map(|(k, n)| D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(n.as_ptr()),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: (k * 16) as u32,
                    InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                    InstanceDataStepRate: 1,
                })
                .collect();
            let mut layout = None;
            device.CreateInputLayout(&elems, blob(&vsb), Some(&mut layout))?;
            if std::env::var("HAYATE_MEMLOG").is_ok() { eprintln!("  gpu shaders {:.1}", crate::sys::process_memory().private_bytes as f64 / 1048576.0); }
            let mut cbuf = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DYNAMIC, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default() },
                None,
                Some(&mut cbuf),
            )?;
            let mut blend = None;
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            device.CreateBlendState(&bd, Some(&mut blend))?;
            let mut sampler = None;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            let mut atlas = None;
            device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: ATLAS,
                    Height: ATLAS,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut atlas),
            )?;
            let atlas = atlas.expect("atlas");
            if std::env::var("HAYATE_MEMLOG").is_ok() { eprintln!("  gpu atlas {:.1}", crate::sys::process_memory().private_bytes as f64 / 1048576.0); }
            let mut atlas_srv = None;
            device.CreateShaderResourceView(&atlas, None, Some(&mut atlas_srv))?;
            let mut g = Gpu {
                device,
                ctx,
                swap,
                rtv: None,
                vs: vs.expect("vs"),
                ps: ps.expect("ps"),
                layout: layout.expect("layout"),
                cbuf: cbuf.expect("cbuf"),
                inst: None,
                inst_cap: 0,
                blend: blend.expect("blend"),
                sampler: sampler.expect("sampler"),
                atlas,
                atlas_srv: atlas_srv.expect("atlas srv"),
                white: None,
                shelf_x: 0,
                shelf_y: 0,
                shelf_h: 0,
                glyphs: HashMap::new(),
                dw: super::text::factory(),
                w: 0,
                h: 0,
                quads: Vec::new(),
                batches: Vec::new(),
                clear: [0.0; 4],
                adapter: name,
                texture_bytes: 0,
            };
            g.grow(4096)?;
            g.white = Some(g.image(1, 1, &[255, 255, 255, 255])?.srv);
            g.texture_bytes = 0;
            Ok(g)
        }
    }

    fn grow(&mut self, n: usize) -> Result<()> {
        let cap = n.next_power_of_two().max(1024);
        let mut b = None;
        unsafe {
            self.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: (cap * std::mem::size_of::<Quad>()) as u32,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut b),
            )?;
        }
        self.inst = b;
        self.inst_cap = cap;
        Ok(())
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == self.w && h == self.h || w == 0 || h == 0 {
            return;
        }
        self.rtv = None;
        unsafe {
            self.ctx.OMSetRenderTargets(None, None);
            let _ = self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0));
        }
        self.w = w;
        self.h = h;
    }

    /// A texture from premultiplied BGRA pixels.
    pub fn image(&mut self, w: u32, h: u32, bgra: &[u8]) -> Result<Image> {
        let mut tex = None;
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: bgra.as_ptr() as *const _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        unsafe {
            self.device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: w.max(1),
                    Height: h.max(1),
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                },
                Some(&data),
                Some(&mut tex),
            )?;
            let tex = tex.expect("texture");
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            self.texture_bytes += (w * h * 4) as usize;
            Ok(Image { srv: srv.expect("srv"), w, h })
        }
    }

    pub fn begin(&mut self, clear: [u8; 4]) {
        self.quads.clear();
        self.batches.clear();
        self.batches.push((0, None));
        self.clear = rgba(clear, 1.0);
    }

    fn set_image(&mut self, img: Option<&ID3D11ShaderResourceView>) {
        let same = match (&self.batches.last().and_then(|b| b.1.as_ref()), img) {
            (None, None) => true,
            (Some(a), Some(b)) => a.as_raw() == b.as_raw(),
            _ => false,
        };
        if !same {
            self.batches.push((self.quads.len(), img.cloned()));
        }
    }

    /// A filled box, optionally rounded. Device pixels.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 4], radius: f32, clip: Clip) {
        if w <= 0.0 || h <= 0.0 || color[3] == 0 {
            return;
        }
        self.quads.push(Quad { rect: [x, y, w, h], color: rgba(color, 1.0), uv: [0.0; 4], prm: [radius.min(w / 2.0).min(h / 2.0), 0.0, 0.0, 0.0], clip });
    }

    /// A border ring of one width.
    pub fn ring(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, color: [u8; 4], radius: f32, clip: Clip) {
        if w <= 0.0 || h <= 0.0 || color[3] == 0 || width <= 0.0 {
            return;
        }
        self.quads.push(Quad { rect: [x, y, w, h], color: rgba(color, 1.0), uv: [0.0; 4], prm: [radius.min(w / 2.0).min(h / 2.0), width, 3.0, 0.0], clip });
    }

    pub fn draw_image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32, alpha: f32, radius: f32, clip: Clip) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        self.set_image(Some(&img.srv));
        self.quads.push(Quad { rect: [x, y, w, h], color: [1.0, 1.0, 1.0, alpha], uv: [0.0, 0.0, 1.0, 1.0], prm: [radius.min(w / 2.0).min(h / 2.0), 0.0, 2.0, 0.0], clip });
        self.set_image(None);
    }

    /// One glyph with its baseline origin at (x, y), device pixels.
    pub fn glyph(&mut self, face: u16, size: f32, id: u16, x: f32, y: f32, color: [f32; 4], clip: Clip) {
        let xf = x.floor();
        let sub = ((x - xf) * 4.0).floor() as u8 & 3;
        let key = GKey { face, size: (size * 16.0).round() as u32, id, sub };
        let slot = match self.glyphs.get(&key) {
            Some(s) => *s,
            None => {
                let s = self.raster(face, size, id, sub);
                self.glyphs.insert(key, s);
                s
            }
        };
        let Some(s) = slot else { return };
        if s.w == 0 {
            return;
        }
        let gx = xf + s.left as f32;
        let gy = y.round() + s.top as f32;
        self.quads.push(Quad {
            rect: [gx, gy, s.w as f32, s.h as f32],
            color,
            uv: [s.x as f32, s.y as f32, (s.x + s.w) as f32, (s.y + s.h) as f32],
            prm: [0.0, 0.0, 1.0, 0.0],
            clip,
        });
    }

    fn raster(&mut self, face: u16, size: f32, id: u16, sub: u8) -> Option<Slot> {
        let f = super::text::face(face)?;
        let adv = 0.0f32;
        let run = DWRITE_GLYPH_RUN {
            fontFace: std::mem::ManuallyDrop::new(Some(f.0.clone())),
            fontEmSize: size,
            glyphCount: 1,
            glyphIndices: &id,
            glyphAdvances: &adv,
            glyphOffsets: std::ptr::null(),
            isSideways: false.into(),
            bidiLevel: 0,
        };
        let r = unsafe {
            let a = self.dw.CreateGlyphRunAnalysis(
                &run,
                None,
                if size < 20.0 { DWRITE_RENDERING_MODE_NATURAL } else { DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC },
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                sub as f32 / 4.0,
                0.0,
            );
            let mut run = run;
            std::mem::ManuallyDrop::drop(&mut run.fontFace);
            a.ok()?
        };
        let b = unsafe { r.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1).ok()? };
        let w = (b.right - b.left).max(0) as u32;
        let h = (b.bottom - b.top).max(0) as u32;
        if w == 0 || h == 0 {
            return Some(Slot { x: 0, y: 0, w: 0, h: 0, left: 0, top: 0 });
        }
        if w > 512 || h > 512 {
            return None;
        }
        let mut px = vec![0u8; (w * h) as usize];
        unsafe { r.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &b, &mut px).ok()? };
        // shelf packing; a full atlas starts over
        if self.shelf_x + w + 1 > ATLAS {
            self.shelf_y += self.shelf_h + 1;
            self.shelf_x = 0;
            self.shelf_h = 0;
        }
        if self.shelf_y + h + 1 > ATLAS {
            self.glyphs.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_h = 0;
        }
        let (sx, sy) = (self.shelf_x, self.shelf_y);
        unsafe {
            let bx = D3D11_BOX { left: sx, top: sy, front: 0, right: sx + w, bottom: sy + h, back: 1 };
            self.ctx.UpdateSubresource(&self.atlas, 0, Some(&bx), px.as_ptr() as *const _, w, 0);
        }
        self.shelf_x += w + 1;
        self.shelf_h = self.shelf_h.max(h);
        Some(Slot { x: sx as u16, y: sy as u16, w: w as u16, h: h as u16, left: b.left as i16, top: b.top as i16 })
    }

    /// Draws the frame and shows it.
    pub fn end(&mut self) -> Result<()> {
        self.draw()?;
        self.present()
    }

    pub fn present(&mut self) -> Result<()> {
        unsafe { self.swap.Present(1, DXGI_PRESENT(0)).ok() }
    }

    /// The drawn frame as top-down BGRA, read back before it is presented.
    pub fn capture(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut st = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut st))?;
            let st = st.expect("staging");
            self.ctx.CopyResource(&st, &back);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let (w, h) = (desc.Width, desc.Height);
            let mut out = vec![0u8; (w * h * 4) as usize];
            for y in 0..h as usize {
                let src = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), (w * 4) as usize);
                out[y * (w * 4) as usize..(y + 1) * (w * 4) as usize].copy_from_slice(src);
            }
            self.ctx.Unmap(&st, 0);
            Ok((w, h, out))
        }
    }

    /// Uploads the frame's quads and draws them.
    pub fn draw(&mut self) -> Result<()> {
        unsafe {
            if self.rtv.is_none() {
                let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
                let mut rtv = None;
                self.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
                self.rtv = rtv;
            }
            let Some(rtv) = self.rtv.clone() else { return Ok(()) };
            if self.quads.len() > self.inst_cap {
                self.grow(self.quads.len())?;
            }
            let ctx = &self.ctx;
            ctx.ClearRenderTargetView(&rtv, &self.clear);
            if !self.quads.is_empty()
                && let Some(inst) = &self.inst
            {
                let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(inst, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
                std::ptr::copy_nonoverlapping(self.quads.as_ptr(), m.pData as *mut Quad, self.quads.len());
                ctx.Unmap(inst, 0);
                let mut cm = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(&self.cbuf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut cm))?;
                *(cm.pData as *mut [f32; 4]) = [self.w as f32, self.h as f32, 0.0, 0.0];
                ctx.Unmap(&self.cbuf, 0);
                ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
                ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: self.w as f32, Height: self.h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
                ctx.OMSetBlendState(&self.blend, None, 0xFFFF_FFFF);
                ctx.IASetInputLayout(&self.layout);
                ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
                let stride = std::mem::size_of::<Quad>() as u32;
                let offset = 0u32;
                ctx.IASetVertexBuffers(0, 1, Some(&Some(inst.clone())), Some(&stride), Some(&offset));
                ctx.VSSetShader(&self.vs, None);
                ctx.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
                ctx.PSSetShader(&self.ps, None);
                ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
                let n = self.quads.len();
                for (k, (start, img)) in self.batches.iter().enumerate() {
                    let end = self.batches.get(k + 1).map_or(n, |b| b.0);
                    if end <= *start {
                        continue;
                    }
                    let t1 = img.clone().or_else(|| self.white.clone());
                    ctx.PSSetShaderResources(0, Some(&[Some(self.atlas_srv.clone()), t1]));
                    ctx.DrawInstanced(4, (end - start) as u32, 0, *start as u32);
                }
            }
        }
        Ok(())
    }
}
