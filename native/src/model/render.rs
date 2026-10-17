//! Drawing a [`Mesh`] with Direct3D 11: one vertex/pixel shader pair (compiled once per process by the inbox
//! `d3dcompiler_47.dll`), Blinn-Phong lighting from a key and a fill light that follow the camera, two-sided
//! (OBJ windings are unreliable), 4x MSAA, opaque parts first and transparent ones blended after them.
//!
//! The same renderer serves the window's live view (on its device, into a texture Direct2D then draws: sRGB
//! for an SDR swap chain, scRGB values at SDR white's level for an FP16 one) and the decode pool's stills (on a
//! WARP device of the worker thread, read back into pixels).

use std::cell::RefCell;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;
use windows::core::{PCSTR, s};

use super::{Image, Mesh, Vertex};
use crate::imaging::{Decoded, VECTOR_MAX_EDGE};

const SHADER: &str = r#"
cbuffer Frame : register(b0) {
    row_major float4x4 view_proj;
    float4 eye;
    float4 key;
    float4 fill;
    float4 up_out; // xyz: world up; w: 0 = encode sRGB, else linear output at this SDR white level
};
cbuffer Material : register(b1) {
    float4 kd_d;
    float4 ks_ns;
    float4 ke;
    float4 flags; // x: diffuse map, y: emission map, z: cut out below this alpha, w: blended
};
Texture2D kd_map : register(t0);
Texture2D ke_map : register(t1);
SamplerState samp : register(s0);

struct VSIn { float3 pos : POSITION; float3 n : NORMAL; float2 uv : TEXCOORD; float3 col : COLOR; };
struct PSIn { float4 pos : SV_Position; float3 world : POSITION; float3 n : NORMAL; float2 uv : TEXCOORD; float3 col : COLOR; };

PSIn vs(VSIn i) {
    PSIn o;
    o.pos = mul(float4(i.pos, 1), view_proj);
    o.world = i.pos;
    o.n = i.n;
    o.uv = float2(i.uv.x, 1 - i.uv.y);
    o.col = i.col;
    return o;
}

float3 encode(float3 c) {
    c = saturate(c);
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(max(c, 1e-6), 1.0 / 2.4) - 0.055;
    return lerp(hi, lo, step(c, 0.0031308));
}

float4 ps(PSIn i) : SV_Target {
    float3 v = normalize(eye.xyz - i.world);
    float3 n = normalize(i.n);
    if (dot(n, v) < 0) n = -n;
    float3 base = kd_d.rgb * i.col;
    float alpha = kd_d.a;
    if (flags.x > 0) {
        float4 t = kd_map.Sample(samp, i.uv);
        base *= t.rgb / max(t.a, 1e-4);
        // The map's alpha counts only where the material says so (an opaque glTF ignores it).
        if (flags.z > 0 || flags.w > 0) alpha *= t.a;
    }
    if (flags.z > 0) {
        if (alpha < flags.z) discard;
        alpha = kd_d.a;
    }
    float hemi = lerp(0.05, 0.16, dot(n, up_out.xyz) * 0.5 + 0.5);
    float lit = saturate(dot(n, key.xyz));
    float diffuse = hemi + lit * 0.85 + saturate(dot(n, fill.xyz)) * 0.3;
    float3 h = normalize(key.xyz + v);
    float3 spec = ks_ns.rgb * pow(saturate(dot(n, h)), ks_ns.w) * (lit > 0 ? 1 : 0);
    float3 glow = ke.rgb;
    if (flags.y > 0) glow = (any(ke.rgb > 0) ? ke.rgb : float3(1, 1, 1)) * ke_map.Sample(samp, i.uv).rgb;
    float3 c = base * diffuse + spec + glow;
    c = up_out.w > 0 ? c * up_out.w : encode(c);
    return float4(c * alpha, alpha);
}
"#;

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(
            SHADER.as_ptr() as _,
            SHADER.len(),
            s!("model.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        );
        let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec();
        if let Err(e) = r {
            let detail = errors.as_ref().map(|b| String::from_utf8_lossy(&bytes(b)).into_owned()).unwrap_or_default();
            return Err(format!("shader: {} {detail}", e.message()));
        }
        code.map(|c| bytes(&c)).ok_or_else(|| "shader: no code".to_string())
    }
}

/// The compiled vertex and pixel shaders (one compile per process, ~20 ms).
fn bytecode() -> Result<&'static (Vec<u8>, Vec<u8>), String> {
    static CODE: OnceLock<Result<(Vec<u8>, Vec<u8>), String>> = OnceLock::new();
    CODE.get_or_init(|| Ok((compile(s!("vs"), s!("vs_4_0"))?, compile(s!("ps"), s!("ps_4_0"))?))).as_ref().map_err(|e| e.clone())
}

// --- Camera ---

/// The narrower of the two fields of view: the model's bounding sphere fills it at zoom 1.
const FOV: f32 = 40.0 * std::f32::consts::PI / 180.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    /// Distance as a multiple of the fitting one.
    pub zoom: f32,
    /// The orbit centre's offset from the model's centre, in model units.
    pub pan: [f32; 3],
}

impl Default for Camera {
    /// Front right, a little above.
    fn default() -> Camera {
        Camera { yaw: 30f32.to_radians(), pitch: 20f32.to_radians(), zoom: 1.0, pan: [0.0; 3] }
    }
}

/// Where the framing goes inside the target: the model is fitted to a frame of aspect `aspect` that covers
/// `scale` of the target's width and height, its centre `offset` from the target's (both in clip space,
/// -1..1). The window's view fits it to the area between the panels while drawing the whole photo layer, so
/// the model carries on under the panels as a photo does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub aspect: f32,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

impl Place {
    /// The framing fills the target.
    pub fn whole(width: u32, height: u32) -> Place {
        Place { aspect: width as f32 / height.max(1) as f32, scale: [1.0; 2], offset: [0.0; 2] }
    }

    /// The frame `inner` inside `outer` (left, top, right, bottom, y down).
    pub fn inside(inner: [f32; 4], outer: [f32; 4]) -> Place {
        let (iw, ih) = ((inner[2] - inner[0]).max(1e-3), (inner[3] - inner[1]).max(1e-3));
        let (ow, oh) = ((outer[2] - outer[0]).max(1e-3), (outer[3] - outer[1]).max(1e-3));
        let cx = (inner[0] + inner[2]) / 2.0 - outer[0];
        let cy = (inner[1] + inner[3]) / 2.0 - outer[1];
        Place { aspect: iw / ih, scale: [iw / ow, ih / oh], offset: [cx / ow * 2.0 - 1.0, 1.0 - cy / oh * 2.0] }
    }
}

struct Eye {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    back: [f32; 3],
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt();
    [v[0] / l, v[1] / l, v[2] / l]
}

fn mix(a: [f32; 3], ka: f32, b: [f32; 3], kb: f32, c: [f32; 3], kc: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] * ka + b[i] * kb + c[i] * kc)
}

impl Camera {
    /// The vertical field of view at an aspect ratio (FOV is the narrower one).
    fn fovy(aspect: f32) -> f32 {
        if aspect >= 1.0 { FOV } else { 2.0 * ((FOV / 2.0).tan() / aspect).atan() }
    }

    /// Unit vectors from the orbit centre to the eye, to its right, and up the screen.
    fn axes(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let back = [self.pitch.cos() * self.yaw.sin(), self.pitch.sin(), self.pitch.cos() * self.yaw.cos()];
        let right = normalized(cross([0.0, 1.0, 0.0], back));
        (back, right, cross(back, right))
    }

    /// The eye's distance from the orbit centre: what fits the model's bounding box from the default angle
    /// (so orbiting doesn't zoom), times the zoom.
    fn distance(&self, mesh: &Mesh, aspect: f32) -> f32 {
        let (back, right, up) = Camera::default().axes();
        let tan_y = (Self::fovy(aspect) / 2.0).tan();
        let tan_x = tan_y * aspect;
        let c = mesh.centre();
        let mut need = mesh.radius() * 0.5;
        for k in 0..8 {
            let p: [f32; 3] = std::array::from_fn(|i| if k >> i & 1 == 1 { mesh.max[i] } else { mesh.min[i] } - c[i]);
            let z = dot(p, back);
            need = need.max(z + dot(p, right).abs() / tan_x).max(z + dot(p, up).abs() / tan_y);
        }
        need * 1.08 * self.zoom
    }

    fn eye(&self, mesh: &Mesh, place: &Place) -> Eye {
        let aspect = place.aspect;
        let r = mesh.radius();
        let c = mesh.centre();
        let target: [f32; 3] = std::array::from_fn(|i| c[i] + self.pan[i]);
        let (back, right, up) = self.axes();
        let dist = self.distance(mesh, aspect);
        let eye = std::array::from_fn(|i| target[i] + back[i] * dist);
        let near = (dist - r * 1.5).max(r * 0.01);
        let far = dist + r * 1.5;
        let f = 1.0 / (Self::fovy(aspect) / 2.0).tan();
        let a = far / (near - far);
        let b = near * far / (near - far);
        // Row vectors: clip = [p, 1] * M (right-handed view space looking down -z, depth 0..1).
        let mut m = [[0.0f32; 4]; 4];
        for i in 0..3 {
            m[i] = [f / aspect * right[i], f * up[i], a * back[i], -back[i]];
        }
        m[3] = [-f / aspect * dot(eye, right), -f * dot(eye, up), -a * dot(eye, back) + b, dot(eye, back)];
        // Into the target: x' = x * scale + w * offset (w is the clip-space divisor), likewise y.
        for row in &mut m {
            row[0] = row[0] * place.scale[0] + row[3] * place.offset[0];
            row[1] = row[1] * place.scale[1] + row[3] * place.offset[1];
        }
        Eye { view_proj: m, eye, right, up, back }
    }

    /// Turns the model under a drag of `dx`, `dy` DIPs.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        // A full turn per ~1250 DIPs of drag.
        const RADIANS_PER_DIP: f32 = 0.005;
        self.yaw -= dx * RADIANS_PER_DIP;
        self.pitch = (self.pitch + dy * RADIANS_PER_DIP).clamp(-89f32.to_radians(), 89f32.to_radians());
    }

    /// Slides the orbit centre with a drag, the model following the pointer, in a view `view_h` DIPs tall.
    pub fn pan_by(&mut self, mesh: &Mesh, dx: f32, dy: f32, view_w: f32, view_h: f32) {
        let aspect = view_w / view_h.max(1.0);
        let (_, right, up) = self.axes();
        let per_dip = 2.0 * self.distance(mesh, aspect) * (Self::fovy(aspect) / 2.0).tan() / view_h.max(1.0);
        for i in 0..3 {
            self.pan[i] += (-dx * right[i] + dy * up[i]) * per_dip;
        }
    }

    pub fn dolly(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(0.02, 50.0);
    }
}

// --- Device resources ---

/// What the pixel shader writes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Output {
    /// sRGB-encoded, for an 8-bit target.
    Srgb,
    /// Linear light with SDR white at this value (scRGB).
    Linear(f32),
}

pub struct Renderer {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    frame_cb: ID3D11Buffer,
    material_cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    raster: ID3D11RasterizerState,
    opaque: ID3D11BlendState,
    blend: ID3D11BlendState,
    /// Depth only (no colour): the pre-pass that keeps only the nearest transparent surface.
    no_colour: ID3D11BlendState,
    depth_write: ID3D11DepthStencilState,
    depth_read: ID3D11DepthStencilState,
}

struct GpuMaterial {
    consts: [f32; 16],
    kd: Option<ID3D11ShaderResourceView>,
    ke: Option<ID3D11ShaderResourceView>,
    transparent: bool,
}

/// A mesh's buffers and textures on one device.
pub struct GpuMesh {
    vb: ID3D11Buffer,
    ib: ID3D11Buffer,
    parts: Vec<super::Part>,
    materials: Vec<GpuMaterial>,
}

/// Where a frame is drawn: an MSAA colour and depth buffer, resolved into `texture` (premultiplied alpha).
pub struct Target {
    pub width: u32,
    pub height: u32,
    pub format: DXGI_FORMAT,
    msaa: Option<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    pub texture: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    depth: ID3D11DepthStencilView,
}

fn buffer(device: &ID3D11Device, bind: D3D11_BIND_FLAG, bytes: &[u8]) -> Result<ID3D11Buffer, String> {
    let desc = D3D11_BUFFER_DESC { ByteWidth: bytes.len() as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: bind.0 as u32, ..Default::default() };
    let data = D3D11_SUBRESOURCE_DATA { pSysMem: bytes.as_ptr() as _, ..Default::default() };
    let mut b = None;
    unsafe { device.CreateBuffer(&desc, Some(&data), Some(&mut b)) }.map_err(err)?;
    b.ok_or_else(|| "no buffer".into())
}

fn as_bytes<T>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

impl Renderer {
    pub fn new(device: &ID3D11Device) -> Result<Renderer, String> {
        let (vs_code, ps_code) = bytecode()?;
        unsafe {
            let ctx = device.GetImmediateContext().map_err(err)?;
            let mut vs = None;
            device.CreateVertexShader(vs_code, None, Some(&mut vs)).map_err(err)?;
            let mut ps = None;
            device.CreatePixelShader(ps_code, None, Some(&mut ps)).map_err(err)?;
            let element = |name: PCSTR, format: DXGI_FORMAT, offset: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: format,
                InputSlot: 0,
                AlignedByteOffset: offset,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let elements = [
                element(s!("POSITION"), DXGI_FORMAT_R32G32B32_FLOAT, 0),
                element(s!("NORMAL"), DXGI_FORMAT_R32G32B32_FLOAT, 12),
                element(s!("TEXCOORD"), DXGI_FORMAT_R32G32_FLOAT, 24),
                element(s!("COLOR"), DXGI_FORMAT_R32G32B32_FLOAT, 32),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, vs_code, Some(&mut layout)).map_err(err)?;
            let frame_cb = buffer(device, D3D11_BIND_CONSTANT_BUFFER, &[0u8; 128])?;
            let material_cb = buffer(device, D3D11_BIND_CONSTANT_BUFFER, &[0u8; 64])?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_ANISOTROPIC,
                AddressU: D3D11_TEXTURE_ADDRESS_WRAP,
                AddressV: D3D11_TEXTURE_ADDRESS_WRAP,
                AddressW: D3D11_TEXTURE_ADDRESS_WRAP,
                MaxAnisotropy: 8,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sd, Some(&mut sampler)).map_err(err)?;
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                MultisampleEnable: true.into(),
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(err)?;
            let blend_state = |on: bool, mask: u8| -> Result<ID3D11BlendState, String> {
                let mut d = D3D11_BLEND_DESC::default();
                d.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: on.into(),
                    SrcBlend: D3D11_BLEND_ONE,
                    DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: mask,
                };
                let mut b = None;
                device.CreateBlendState(&d, Some(&mut b)).map_err(err)?;
                b.ok_or_else(|| "no blend state".into())
            };
            let depth_state = |write: bool| -> Result<ID3D11DepthStencilState, String> {
                let d = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: true.into(),
                    DepthWriteMask: if write { D3D11_DEPTH_WRITE_MASK_ALL } else { D3D11_DEPTH_WRITE_MASK_ZERO },
                    DepthFunc: D3D11_COMPARISON_LESS_EQUAL,
                    ..Default::default()
                };
                let mut s = None;
                device.CreateDepthStencilState(&d, Some(&mut s)).map_err(err)?;
                s.ok_or_else(|| "no depth state".into())
            };
            Ok(Renderer {
                device: device.clone(),
                ctx,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                frame_cb,
                material_cb,
                sampler: sampler.unwrap(),
                raster: raster.unwrap(),
                opaque: blend_state(false, D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8)?,
                blend: blend_state(true, D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8)?,
                no_colour: blend_state(false, 0)?,
                depth_write: depth_state(true)?,
                depth_read: depth_state(false)?,
            })
        }
    }

    fn texture(&self, img: &Image) -> Result<ID3D11ShaderResourceView, String> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC {
                Width: img.width,
                Height: img.height,
                MipLevels: 0,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
            };
            let mut tex = None;
            if self.device.CreateTexture2D(&desc, None, Some(&mut tex)).is_ok() {
                let tex = tex.unwrap();
                self.ctx.UpdateSubresource(&tex, 0, None, img.pixels.as_ptr() as _, img.width * 4, 0);
                let mut srv = None;
                self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)).map_err(err)?;
                let srv = srv.unwrap();
                self.ctx.GenerateMips(&srv);
                return Ok(srv);
            }
            // No mipmap generation for this format here: one level.
            desc.MipLevels = 1;
            desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            desc.MiscFlags = 0;
            let data = D3D11_SUBRESOURCE_DATA { pSysMem: img.pixels.as_ptr() as _, SysMemPitch: img.width * 4, SysMemSlicePitch: 0 };
            let mut tex = None;
            self.device.CreateTexture2D(&desc, Some(&data), Some(&mut tex)).map_err(err)?;
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex.unwrap(), None, Some(&mut srv)).map_err(err)?;
            Ok(srv.unwrap())
        }
    }

    pub fn upload(&self, mesh: &Mesh) -> Result<GpuMesh, String> {
        let vb = buffer(&self.device, D3D11_BIND_VERTEX_BUFFER, as_bytes(&mesh.vertices))?;
        let ib = buffer(&self.device, D3D11_BIND_INDEX_BUFFER, as_bytes(&mesh.indices))?;
        let mut cache: Vec<(*const Image, ID3D11ShaderResourceView)> = Vec::new();
        let mut srv = |img: &Option<Arc<Image>>| -> Option<ID3D11ShaderResourceView> {
            let img = img.as_ref()?;
            let key = Arc::as_ptr(img);
            if let Some((_, v)) = cache.iter().find(|(k, _)| *k == key) {
                return Some(v.clone());
            }
            let v = self.texture(img).map_err(|e| crate::trace::mark(format!("model: texture upload failed: {e}"))).ok()?;
            cache.push((key, v.clone()));
            Some(v)
        };
        let materials = mesh
            .materials
            .iter()
            .map(|m| {
                let kd = srv(&m.map_kd);
                let ke = srv(&m.map_ke);
                let flag = |b: bool| if b { 1.0 } else { 0.0 };
                let consts = [
                    m.kd[0], m.kd[1], m.kd[2], m.d, m.ks[0], m.ks[1], m.ks[2], m.ns, m.ke[0], m.ke[1], m.ke[2], 0.0,
                    flag(kd.is_some()), flag(ke.is_some()), if m.cutout && kd.is_some() { m.cutoff.max(1e-3) } else { 0.0 }, flag(m.blend),
                ];
                GpuMaterial { consts, kd, ke, transparent: m.transparent() }
            })
            .collect();
        Ok(GpuMesh { vb, ib, parts: mesh.parts.clone(), materials })
    }

    /// A target of `width × height` pixels in `format` (B8G8R8A8_UNORM or R16G16B16A16_FLOAT).
    pub fn target(&self, width: u32, height: u32, format: DXGI_FORMAT) -> Result<Target, String> {
        unsafe {
            let samples = if self.device.CheckMultisampleQualityLevels(format, 4).unwrap_or(0) > 0 { 4 } else { 1 };
            let tex = |samples: u32, format: DXGI_FORMAT, bind: i32| -> Result<ID3D11Texture2D, String> {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: samples, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: bind as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut t)).map_err(err)?;
                t.ok_or_else(|| "no texture".into())
            };
            let rtv_of = |t: &ID3D11Texture2D| -> Result<ID3D11RenderTargetView, String> {
                let mut v = None;
                self.device.CreateRenderTargetView(t, None, Some(&mut v)).map_err(err)?;
                v.ok_or_else(|| "no view".into())
            };
            let texture = tex(1, format, D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0)?;
            let rtv = rtv_of(&texture)?;
            let msaa = if samples > 1 {
                let t = tex(samples, format, D3D11_BIND_RENDER_TARGET.0)?;
                let v = rtv_of(&t)?;
                Some((t, v))
            } else {
                None
            };
            let depth_tex = tex(samples, DXGI_FORMAT_D32_FLOAT, D3D11_BIND_DEPTH_STENCIL.0)?;
            let mut depth = None;
            self.device.CreateDepthStencilView(&depth_tex, None, Some(&mut depth)).map_err(err)?;
            Ok(Target { width, height, format, msaa, texture, rtv, depth: depth.unwrap() })
        }
    }

    /// Draws `mesh` seen by `camera` into `target`, on a transparent background.
    pub fn draw(&self, mesh: &Mesh, gpu: &GpuMesh, target: &Target, camera: &Camera, place: &Place, out: Output) {
        let e = camera.eye(mesh, place);
        // Lights hang off the camera: the key above left and in front, a dim fill from the right.
        let key = normalized(mix(e.right, -0.5, e.up, 0.7, e.back, 0.6));
        let fill = normalized(mix(e.right, 0.7, e.up, -0.2, e.back, 0.5));
        let mut frame = [0f32; 32];
        for r in 0..4 {
            frame[r * 4..r * 4 + 4].copy_from_slice(&e.view_proj[r]);
        }
        frame[16..19].copy_from_slice(&e.eye);
        frame[20..23].copy_from_slice(&key);
        frame[24..27].copy_from_slice(&fill);
        frame[28..32].copy_from_slice(&[0.0, 1.0, 0.0, match out {
            Output::Srgb => 0.0,
            Output::Linear(white) => white,
        }]);
        unsafe {
            let ctx = &self.ctx;
            let rtv = target.msaa.as_ref().map_or(&target.rtv, |(_, v)| v);
            ctx.ClearRenderTargetView(rtv, &[0.0; 4]);
            ctx.ClearDepthStencilView(&target.depth, D3D11_CLEAR_DEPTH.0, 1.0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), &target.depth);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: target.width as f32,
                Height: target.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(&self.layout);
            let stride = std::mem::size_of::<Vertex>() as u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(gpu.vb.clone())), Some(&stride), Some(&0));
            ctx.IASetIndexBuffer(&gpu.ib, DXGI_FORMAT_R32_UINT, 0);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.UpdateSubresource(&self.frame_cb, 0, None, frame.as_ptr() as _, 0, 0);
            let cbs = [Some(self.frame_cb.clone()), Some(self.material_cb.clone())];
            ctx.VSSetConstantBuffers(0, Some(&cbs));
            ctx.PSSetConstantBuffers(0, Some(&cbs));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            // Opaque parts; then the transparent ones' depth alone, so that blending shows only the nearest
            // transparent surface (unsorted layers of one object blended in a jumble of orders).
            let passes = [(false, &self.opaque, &self.depth_write), (true, &self.no_colour, &self.depth_write), (true, &self.blend, &self.depth_read)];
            for (transparent, blend, depth) in passes {
                ctx.OMSetBlendState(blend, None, u32::MAX);
                ctx.OMSetDepthStencilState(depth, 0);
                for p in &gpu.parts {
                    let m = &gpu.materials[p.material];
                    if m.transparent != transparent {
                        continue;
                    }
                    ctx.UpdateSubresource(&self.material_cb, 0, None, m.consts.as_ptr() as _, 0, 0);
                    ctx.PSSetShaderResources(0, Some(&[m.kd.clone(), m.ke.clone()]));
                    ctx.DrawIndexed(p.count, p.start, 0);
                }
            }
            if let Some((t, _)) = &target.msaa {
                ctx.ResolveSubresource(&target.texture, 0, t, 0, target.format);
            }
            // Leave nothing bound that Direct2D will read.
            ctx.OMSetRenderTargets(None, None);
            ctx.PSSetShaderResources(0, Some(&[None, None]));
        }
    }
}

// --- Stills for the decode pool ---

thread_local! {
    static WARP: RefCell<Option<Renderer>> = const { RefCell::new(None) };
}

fn warp_renderer<R>(f: impl FnOnce(&Renderer) -> Result<R, String>) -> Result<R, String> {
    WARP.with(|cell| {
        if cell.borrow().is_none() {
            let mut device = None;
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
            }
            .map_err(err)?;
            *cell.borrow_mut() = Some(Renderer::new(&device.ok_or("no WARP device")?)?);
        }
        f(cell.borrow().as_ref().unwrap())
    })
}

/// The still's aspect (thumbnails and the first frame, before the live view takes over).
const STILL_ASPECT: f32 = 4.0 / 3.0;

/// A still of the model from the default camera, fitted to the box, for the decode pool.
pub fn still(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let mesh = super::load(f, path)?;
    let (bw, bh) = if box_w == 0 || box_h == 0 { (2048, 1536) } else { (box_w.min(VECTOR_MAX_EDGE), box_h.min(VECTOR_MAX_EDGE)) };
    let w = (bw as f32).min(bh as f32 * STILL_ASPECT).round().max(16.0) as u32;
    let h = (w as f32 / STILL_ASPECT).round().max(12.0) as u32;
    let pixels = warp_renderer(|r| {
        let gpu = r.upload(&mesh)?;
        let target = r.target(w, h, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        r.draw(&mesh, &gpu, &target, &Camera::default(), &Place::whole(w, h), Output::Srgb);
        read_back(r, &target)
    })?;
    let mut d = Decoded::still(w, h, pixels, 1600, 1200, None);
    d.vector = true;
    Ok(d)
}

fn read_back(r: &Renderer, target: &Target) -> Result<Vec<u8>, String> {
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: target.width,
            Height: target.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: target.format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        r.device.CreateTexture2D(&desc, None, Some(&mut staging)).map_err(err)?;
        let staging = staging.unwrap();
        r.ctx.CopyResource(&staging, &target.texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        r.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(err)?;
        let row = target.width as usize * 4;
        let mut px = vec![0u8; row * target.height as usize];
        for y in 0..target.height as usize {
            let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
            px[y * row..(y + 1) * row].copy_from_slice(src);
        }
        r.ctx.Unmap(&staging, 0);
        Ok(px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(min: [f32; 3], max: [f32; 3]) -> Mesh {
        Mesh { vertices: Vec::new(), indices: Vec::new(), parts: Vec::new(), materials: Vec::new(), min, max }
    }

    fn project(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
        let c: [f32; 4] = std::array::from_fn(|j| p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j]);
        [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
    }

    #[test]
    fn the_centre_projects_to_the_middle_and_the_sphere_fits() {
        let m = mesh([-1.0, -2.0, -3.0], [3.0, 2.0, 1.0]);
        for aspect in [0.5f32, 1.0, 2.0] {
            let e = Camera::default().eye(&m, &Place { aspect, scale: [1.0; 2], offset: [0.0; 2] });
            let c = project(&e.view_proj, m.centre());
            assert!(c[0].abs() < 1e-4 && c[1].abs() < 1e-4 && (0.0..1.0).contains(&c[2]), "{c:?}");
            // A point on the sphere straight up (screen-wise) from the centre lands inside the view.
            let r = m.radius();
            let top: [f32; 3] = std::array::from_fn(|i| m.centre()[i] + e.up[i] * r);
            let t = project(&e.view_proj, top);
            assert!(t[1] > 0.0 && t[1] <= 1.0, "aspect {aspect}: {t:?}");
        }
    }

    /// Renders the model named by `LOOKER_DECODE` to `<it>.png` beside it.
    /// `LOOKER_DECODE=x.obj cargo test --release -- --ignored --nocapture renders_a_still`
    #[test]
    #[ignore]
    fn renders_a_still() {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let p = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let t = std::time::Instant::now();
        let d = still(&f, &p, 1600, 1200).unwrap();
        println!("{}x{} in {:.0} ms", d.width, d.height, t.elapsed().as_secs_f64() * 1000.0);
        let t = std::time::Instant::now();
        still(&f, &p, 1600, 1200).unwrap();
        println!("again (mesh cached) in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        let rgba: Vec<u8> = d.frames[0].pixels.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], c[3]]).collect();
        image::save_buffer(p.with_extension("png"), &rgba, d.width, d.height, image::ExtendedColorType::Rgba8).unwrap();
    }

    #[test]
    fn a_place_inside_maps_the_frame_into_the_target() {
        let m = mesh([-1.0; 3], [1.0; 3]);
        // A frame on the right half of a 200 x 100 target: the centre lands at x = +0.5.
        let p = Place::inside([100.0, 0.0, 200.0, 100.0], [0.0, 0.0, 200.0, 100.0]);
        assert_eq!((p.aspect, p.scale, p.offset), (1.0, [0.5, 1.0], [0.5, 0.0]));
        let e = Camera::default().eye(&m, &p);
        let c = project(&e.view_proj, m.centre());
        assert!((c[0] - 0.5).abs() < 1e-4 && c[1].abs() < 1e-4, "{c:?}");
    }

    #[test]
    fn orbit_keeps_the_pitch_short_of_the_poles() {
        let mut c = Camera::default();
        c.orbit(0.0, 10_000.0);
        assert!(c.pitch < 89.5f32.to_radians());
        c.orbit(0.0, -20_000.0);
        assert!(c.pitch > -89.5f32.to_radians());
    }
}
