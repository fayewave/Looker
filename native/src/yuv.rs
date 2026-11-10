//! A decoded video picture to an upright RGB texture Direct2D draws: one pixel shader for every layout the
//! decoder hands out (looker_video.c's LvFrame). A GPU picture (a slice of the decoder's NV12/P010 texture array)
//! is copied into a texture of ours first (the decoder's can't be sampled). CPU planes are uploaded by the decode
//! thread as soon as they're decoded (`Uploader`: Map and copy there, so a 33 MB 4K ProRes picture never costs the
//! UI thread its frame), or here when there was no device to upload to.
//!
//! The shader applies the stream's YUV matrix and range, turns the picture upright (the container's rotation),
//! decodes its transfer (the sRGB curve for SDR, PQ or HLG for HDR, SDR white = BT.2408's 203 nits), converts its
//! primaries to Rec. 709's, rolls HDR highlights off into the display's headroom (all of it into SDR white on an SDR
//! display), premultiplies alpha, and writes either 8-bit sRGB-encoded values (an SDR swap chain) or linear scRGB at
//! SDR white's level (FP16, the HDR / Auto Color Management one), like the 3D view (model/render.rs). The still
//! (looker_video.c's `hdr_to_sdr`) uses the same curve with a headroom of 1.

use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, PCSTR, s};

use crate::imaging::hdr::{self, Matrix};
use crate::imaging::video::{LvFrame, PLANAR, RGBA, TEXTURE};

const SHADER: &str = r#"
cbuffer C : register(b0) {
    float4 r0, r1, r2;  // rgb = (r0, r1, r2) . (y, u, v, 1)
    float4 rot0, rot1;  // source uv = (rot0, rot1) . (u, v, 1): the turn to upright
    float4 opts;        // x: 0 three planes, 1 Y + interleaved UV, 2 RGBA; y: alpha plane; z: linear output at this white (0 = encoded); w: sample scale
    float4 crop;        // xy: the picture's share of the texture
    float4 g0, g1, g2;  // linear light in the stream's primaries to Rec. 709's
    float4 hdr;         // x: transfer 0 SDR, 1 PQ, 2 HLG; y: the headroom to roll off into (in SDR whites); z: the knee; w: 1 = through linear light even into 8 bits
}
Texture2D tY : register(t0);
Texture2D tU : register(t1);
Texture2D tV : register(t2);
Texture2D tA : register(t3);
SamplerState smp : register(s0);
struct VO { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VO vs(uint id : SV_VertexID) {
    VO o;
    float2 t = float2((id << 1) & 2, id & 2);
    o.pos = float4(t * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = t;
    return o;
}
float lin(float c) { return c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4); }
float enc(float c) { return c <= 0.0031308 ? c * 12.92 : 1.055 * pow(c, 1.0 / 2.4) - 0.055; }
// SMPTE ST 2084 to linear light, SDR white (203 nits) = 1.
float3 pq(float3 e) {
    float3 p = pow(saturate(e), 1.0 / 78.84375);
    return pow(max(p - 0.8359375, 0) / (18.8515625 - 18.6875 * p), 1.0 / 0.1593017578125) * (10000.0 / 203.0);
}
// BT.2100 HLG to linear light, SDR white = 1: the inverse OETF, then the OOTF of the nominal 1000-nit display.
float3 hlg(float3 e) {
    e = saturate(e);
    float3 s = e <= 0.5 ? e * e / 3.0 : (exp((e - 0.55991073) / 0.17883277) + 0.28466892) / 12.0;
    float y = dot(float3(0.2627, 0.6780, 0.0593), s);
    return s * pow(max(y, 1e-6), 0.2) * (1000.0 / 203.0);
}
// Below the knee as it is; above, the brightest channel eased towards the headroom (the hue kept).
float3 tone(float3 c) {
    float m = max(c.r, max(c.g, c.b));
    float room = max(hdr.y - hdr.z, 1e-4);
    if (m > hdr.z) c *= (hdr.z + room * (1 - exp(-(m - hdr.z) / room))) / m;
    return c;
}
float4 ps(VO i) : SV_Target {
    float3 q = float3(i.uv, 1);
    float2 src = float2(dot(rot0.xyz, q), dot(rot1.xyz, q)) * crop.xy;
    float4 c;
    if (opts.x > 1.5) {
        c = tY.Sample(smp, src);
    } else {
        float y = tY.Sample(smp, src).r * opts.w;
        float2 uv = opts.x > 0.5 ? tU.Sample(smp, src).rg * opts.w : float2(tU.Sample(smp, src).r, tV.Sample(smp, src).r) * opts.w;
        float4 yuv = float4(y, uv, 1);
        c.rgb = saturate(float3(dot(r0, yuv), dot(r1, yuv), dot(r2, yuv)));
        c.a = opts.y > 0.5 ? saturate(tA.Sample(smp, src).r * opts.w) : 1;
    }
    if (opts.z > 0 || hdr.w > 0.5) {
        float3 l = hdr.x > 1.5 ? hlg(c.rgb) : (hdr.x > 0.5 ? pq(c.rgb) : float3(lin(c.r), lin(c.g), lin(c.b)));
        l = float3(dot(g0.xyz, l), dot(g1.xyz, l), dot(g2.xyz, l));
        if (hdr.x > 0.5) l = tone(l);
        c.rgb = opts.z > 0 ? l * opts.z : float3(enc(saturate(l.r)), enc(saturate(l.g)), enc(saturate(l.b)));
    }
    c.rgb *= c.a;
    return c;
}
"#;

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(SHADER.as_ptr() as _, SHADER.len(), s!("yuv.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors));
        let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec();
        if let Err(e) = r {
            let detail = errors.as_ref().map(|b| String::from_utf8_lossy(&bytes(b)).into_owned()).unwrap_or_default();
            return Err(format!("shader: {} {detail}", e.message()));
        }
        code.map(|c| bytes(&c)).ok_or_else(|| "shader: no code".to_string())
    }
}

fn bytecode() -> Result<&'static (Vec<u8>, Vec<u8>), String> {
    static CODE: OnceLock<Result<(Vec<u8>, Vec<u8>), String>> = OnceLock::new();
    CODE.get_or_init(|| Ok((compile(s!("vs"), s!("vs_4_0"))?, compile(s!("ps"), s!("ps_4_0"))?))).as_ref().map_err(|e| e.clone())
}

/// The YUV to RGB rows (`r0..r2` in the shader) for FFmpeg's AVCOL_SPC `matrix`, with `full` range, for samples
/// of `depth` bits read as 0..1.
pub fn yuv_rows(matrix: i32, full: bool, depth: u32, height: u32) -> [[f32; 4]; 3] {
    let (kr, kb) = match matrix {
        1 => (0.2126, 0.0722),        // BT.709
        5 | 6 => (0.299, 0.114),      // BT.470BG, SMPTE 170M (601)
        7 => (0.212, 0.087),          // SMPTE 240M
        4 => (0.30, 0.11),            // FCC
        9 | 10 => (0.2627, 0.0593),   // BT.2020
        _ if height >= 720 => (0.2126, 0.0722),
        _ => (0.299, 0.114),
    };
    let kg = 1.0 - kr - kb;
    let max = ((1u32 << depth) - 1) as f64;
    let unit = (1u32 << depth.saturating_sub(8)) as f64; // one 8-bit code at this depth
    let (y_off, y_scale, c_off, c_scale) = if full {
        (0.0, 1.0, (1u32 << (depth - 1)) as f64 / max, 1.0)
    } else {
        (16.0 * unit / max, max / (219.0 * unit), 128.0 * unit / max, max / (224.0 * unit))
    };
    // R = Y + 2(1-Kr) V; G = Y - 2Kb(1-Kb)/Kg U - 2Kr(1-Kr)/Kg V; B = Y + 2(1-Kb) U (Y, U, V centred and scaled).
    let rv = 2.0 * (1.0 - kr);
    let gu = -2.0 * kb * (1.0 - kb) / kg;
    let gv = -2.0 * kr * (1.0 - kr) / kg;
    let bu = 2.0 * (1.0 - kb);
    let row = |u: f64, v: f64| -> [f32; 4] {
        let (ay, au, av) = (y_scale, u * c_scale, v * c_scale);
        [ay as f32, au as f32, av as f32, (-ay * y_off - au * c_off - av * c_off) as f32]
    };
    [row(0.0, rv), row(gu, gv), row(bu, 0.0)]
}

/// Source uv from upright uv for a picture turned `rotation` degrees clockwise to stand upright.
fn turn(rotation: i32) -> ([f32; 4], [f32; 4]) {
    match rotation {
        90 => ([0.0, 1.0, 0.0, 0.0], [-1.0, 0.0, 1.0, 0.0]),
        180 => ([-1.0, 0.0, 1.0, 0.0], [0.0, -1.0, 1.0, 0.0]),
        270 => ([0.0, -1.0, 1.0, 0.0], [1.0, 0.0, 0.0, 0.0]),
        _ => ([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]),
    }
}

// --- Colour: transfer, primaries, the highlight rolloff ---

/// The rolloff's knee as a share of the headroom: light below it is shown as it is.
pub const KNEE: f32 = 0.75;

/// The brightest channel `m` (1 = SDR white) rolled off into `headroom` (the shader's `tone`, and
/// looker_video.c's for the stills).
#[cfg(test)]
pub fn tone(m: f32, headroom: f32) -> f32 {
    let k = KNEE * headroom;
    if m <= k {
        return m;
    }
    let room = (headroom - k).max(1e-4);
    k + room * (1.0 - (-(m - k) / room).exp())
}

/// FFmpeg's AVCOL_TRC as the shader's transfer: 1 PQ, 2 HLG, 0 anything else (the sRGB curve).
pub fn transfer_kind(trc: i32) -> u32 {
    match trc {
        16 => 1,
        18 => 2,
        _ => 0,
    }
}

/// Whether AVCOL_TRC `trc` is an HDR one (PQ, HLG).
pub fn is_hdr(trc: i32) -> bool {
    transfer_kind(trc) != 0
}

/// Linear light in AVCOL_PRI `primaries` to Rec. 709's. HDR with no tag is BT.2020 (which it always is).
pub fn gamut(primaries: i32, hdr_video: bool) -> Matrix {
    match primaries {
        9 => hdr::BT2020_TO_709,
        11 | 12 => hdr::P3_TO_709,
        2 if hdr_video => hdr::BT2020_TO_709,
        _ => hdr::IDENTITY,
    }
}

/// Whether a stream needs more than SDR Rec. 709 (drawn without the SDR clamp under scRGB).
pub fn is_wide(transfer: i32, primaries: i32) -> bool {
    is_hdr(transfer) || gamut(primaries, false) != hdr::IDENTITY
}

/// How a picture's light reaches the output texture.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    /// Linear FP16 at this SDR white (scRGB), or None for 8-bit sRGB-encoded values.
    pub white: Option<f32>,
    /// How far above SDR white the output may go (1 on an SDR display): HDR highlights are rolled off into it.
    pub headroom: f32,
    /// The stream's AVCOL_TRC and AVCOL_PRI (the picture's own, or the container's when it doesn't say).
    pub transfer: i32,
    pub primaries: i32,
}

// --- Planes: CPU pictures on the GPU ---

struct Plane {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
}
// SAFETY: D3D11 resources are free-threaded; the device is made multithread-protected (gfx.rs).
unsafe impl Send for Plane {}

/// One plane of a CPU picture: its size and texture format, and where its rows are.
struct Spec {
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    data: *const u8,
    stride: usize,
    row: usize,
}

/// The textures a CPU picture needs, in the shader's slot order.
fn plane_specs(f: &LvFrame) -> Result<Vec<Spec>, String> {
    let (w, h) = (f.width.max(1) as u32, f.height.max(1) as u32);
    match f.layout {
        PLANAR => {
            let deep = f.bit_depth > 8;
            let (cw, ch) = ((w + (1 << f.chroma_w_shift) - 1) >> f.chroma_w_shift, (h + (1 << f.chroma_h_shift) - 1) >> f.chroma_h_shift);
            let one = if deep { DXGI_FORMAT_R16_UNORM } else { DXGI_FORMAT_R8_UNORM };
            let two = if deep { DXGI_FORMAT_R16G16_UNORM } else { DXGI_FORMAT_R8G8_UNORM };
            let b = if deep { 2 } else { 1 };
            let plane = |i: usize, w: u32, h: u32, format, per: usize| Spec { w, h, format, data: f.data[i], stride: f.linesize[i] as usize, row: w as usize * per };
            let mut v = vec![plane(0, w, h, one, b)];
            if f.nplanes == 2 {
                v.push(plane(1, cw, ch, two, b * 2));
            } else {
                v.push(plane(1, cw, ch, one, b));
                v.push(plane(2, cw, ch, one, b));
                if f.nplanes == 4 {
                    v.push(plane(3, w, h, one, b));
                }
            }
            if v.iter().any(|s| s.data.is_null()) {
                return Err("picture planes already released".into());
            }
            Ok(v)
        }
        RGBA if !f.data[0].is_null() => Ok(vec![Spec { w, h, format: DXGI_FORMAT_R8G8B8A8_UNORM, data: f.data[0], stride: f.linesize[0] as usize, row: w as usize * 4 }]),
        l => Err(format!("no planes to upload in layout {l}")),
    }
}

/// The shader's layout mode, whether there's an alpha plane, and how many views it reads.
fn layout_of(f: &LvFrame) -> (f32, f32, usize) {
    match f.layout {
        TEXTURE => (1.0, 0.0, 2),
        RGBA => (2.0, 0.0, 1),
        _ => (if f.nplanes == 2 { 1.0 } else { 0.0 }, if f.nplanes == 4 { 1.0 } else { 0.0 }, f.nplanes.max(2) as usize),
    }
}

fn srv(device: &ID3D11Device, tex: &ID3D11Texture2D, format: DXGI_FORMAT) -> Result<ID3D11ShaderResourceView, String> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
    };
    let mut v = None;
    unsafe { device.CreateShaderResourceView(tex, Some(&desc), Some(&mut v)) }.map_err(err)?;
    v.ok_or_else(|| "no view".into())
}

/// A dynamic texture of `w × h` in `format` the CPU writes and the shader reads.
fn make_plane(device: &ID3D11Device, w: u32, h: u32, format: DXGI_FORMAT) -> Result<Plane, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
    };
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(err)?;
    let tex = t.ok_or("no texture")?;
    let srv = srv(device, &tex, format)?;
    Ok(Plane { tex, srv, w, h, format })
}

/// Copies a plane's rows into its texture. The copy runs between Map and Unmap, outside the device's lock: the
/// UI thread draws meanwhile.
fn fill(ctx: &ID3D11DeviceContext, p: &Plane, s: &Spec) -> Result<(), String> {
    unsafe {
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&p.tex, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).map_err(err)?;
        let dst = m.pData as *mut u8;
        let pitch = m.RowPitch as usize;
        let n = s.row.min(pitch).min(s.stride);
        if pitch == s.stride && n == s.row {
            std::ptr::copy_nonoverlapping(s.data, dst, s.stride * (s.h as usize - 1) + n);
        } else {
            for y in 0..s.h as usize {
                std::ptr::copy_nonoverlapping(s.data.add(y * s.stride), dst.add(y * pitch), n);
            }
        }
        ctx.Unmap(&p.tex, 0);
    }
    Ok(())
}

type Pool = Arc<Mutex<Vec<Plane>>>;
/// Planes kept for reuse once their picture is drawn (a 4K 4:2:2 picture has three).
const POOL_CAP: usize = 12;

/// A CPU picture's planes on the GPU, filled by the decode thread. Given back to the pool on drop.
pub struct Uploaded {
    planes: Vec<Plane>,
    pool: Pool,
}

impl Drop for Uploaded {
    fn drop(&mut self) {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        for p in self.planes.drain(..) {
            if pool.len() < POOL_CAP {
                pool.push(p);
            }
        }
    }
}

/// Puts CPU pictures on the GPU from the decode thread.
pub struct Uploader {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    pool: Pool,
}
// SAFETY: see Plane; one thread (the decode thread) uses it.
unsafe impl Send for Uploader {}

impl Uploader {
    pub fn new(device: &ID3D11Device) -> Result<Uploader, String> {
        let ctx = unsafe { device.GetImmediateContext() }.map_err(err)?;
        Ok(Uploader { device: device.clone(), ctx, pool: Arc::new(Mutex::new(Vec::new())) })
    }

    /// A copy of a GPU picture (a slice of the decoder's texture array) in a texture of its own, `reuse`d when it
    /// matches: kept for scrubbing back (player.rs). The copy is queued on the GPU; the decoder's surface can go.
    pub fn copy_surface(&self, f: &LvFrame, reuse: Option<ID3D11Texture2D>) -> Result<ID3D11Texture2D, String> {
        let src = unsafe { ID3D11Texture2D::from_raw_borrowed(&f.texture) }.ok_or("no texture")?;
        let mut d = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut d) };
        let fits = |t: &ID3D11Texture2D| {
            let mut e = D3D11_TEXTURE2D_DESC::default();
            unsafe { t.GetDesc(&mut e) };
            (e.Width, e.Height, e.Format) == (d.Width, d.Height, d.Format)
        };
        let dst = match reuse.filter(fits) {
            Some(t) => t,
            None => {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: d.Width,
                    Height: d.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: d.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: 0,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(err)?;
                t.ok_or("no texture")?
            }
        };
        unsafe { self.ctx.CopySubresourceRegion(&dst, 0, 0, 0, 0, src, f.texture_index.max(0) as u32, None) };
        Ok(dst)
    }

    /// The bytes a copy of `f` takes on the GPU.
    pub fn surface_bytes(f: &LvFrame) -> usize {
        let Some(src) = (unsafe { ID3D11Texture2D::from_raw_borrowed(&f.texture) }) else { return 0 };
        let mut d = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut d) };
        let per = if d.Format == DXGI_FORMAT_P010 { 2 } else { 1 };
        d.Width as usize * d.Height as usize * per * 3 / 2
    }

    /// `f`'s planes (a PLANAR or RGBA picture) in textures of their own.
    pub fn upload(&self, f: &LvFrame) -> Result<Uploaded, String> {
        let specs = plane_specs(f)?;
        let mut up = Uploaded { planes: Vec::with_capacity(specs.len()), pool: self.pool.clone() };
        for s in &specs {
            let reused = {
                let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
                pool.iter().position(|p| (p.w, p.h, p.format) == (s.w, s.h, s.format)).map(|i| pool.swap_remove(i))
            };
            let p = match reused {
                Some(p) => p,
                None => make_plane(&self.device, s.w, s.h, s.format)?,
            };
            fill(&self.ctx, &p, s)?;
            up.planes.push(p);
        }
        Ok(up)
    }
}

// --- The converter (UI thread) ---

/// Our copy of a GPU picture: the decoder texture's size and format, with views on its two planes.
struct Copy {
    tex: ID3D11Texture2D,
    y: ID3D11ShaderResourceView,
    uv: ID3D11ShaderResourceView,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
}

pub struct Output {
    pub texture: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    pub width: u32,
    pub height: u32,
    pub format: DXGI_FORMAT,
}

pub struct Converter {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    raster: ID3D11RasterizerState,
    /// Planes for pictures that came without an upload (no device on the decode thread).
    planes: Vec<Plane>,
    copy: Option<Copy>,
    pub out: Option<Output>,
}

/// The constant buffer: 11 float4s.
const CB_FLOATS: usize = 44;

impl Converter {
    pub fn new(device: &ID3D11Device) -> Result<Converter, String> {
        let (vs_code, ps_code) = bytecode()?;
        unsafe {
            let ctx = device.GetImmediateContext().map_err(err)?;
            let mut vs = None;
            device.CreateVertexShader(vs_code, None, Some(&mut vs)).map_err(err)?;
            let mut ps = None;
            device.CreatePixelShader(ps_code, None, Some(&mut ps)).map_err(err)?;
            let desc = D3D11_BUFFER_DESC { ByteWidth: (CB_FLOATS * 4) as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            let mut cb = None;
            device.CreateBuffer(&desc, None, Some(&mut cb)).map_err(err)?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sd, Some(&mut sampler)).map_err(err)?;
            let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() };
            let mut raster = None;
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(err)?;
            Ok(Converter {
                device: device.clone(),
                ctx,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                cb: cb.unwrap(),
                sampler: sampler.unwrap(),
                raster: raster.unwrap(),
                planes: Vec::new(),
                copy: None,
                out: None,
            })
        }
    }

    /// Plane slot `i` holding `s` (re-made only when its size or format changes).
    fn upload(&mut self, i: usize, s: &Spec) -> Result<(), String> {
        if self.planes.get(i).is_none_or(|p| (p.w, p.h, p.format) != (s.w, s.h, s.format)) {
            let plane = make_plane(&self.device, s.w, s.h, s.format)?;
            if i < self.planes.len() {
                self.planes[i] = plane;
            } else {
                self.planes.push(plane);
            }
        }
        fill(&self.ctx, &self.planes[i], s)
    }

    /// Copies a GPU picture's array slice into our own texture of the same size and format.
    fn copy_texture(&mut self, f: &LvFrame) -> Result<(), String> {
        let src = unsafe { ID3D11Texture2D::from_raw_borrowed(&f.texture) }.ok_or("no texture")?;
        let mut d = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut d) };
        if self.copy.as_ref().is_none_or(|c| (c.w, c.h, c.format) != (d.Width, d.Height, d.Format)) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: d.Width,
                Height: d.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: d.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(err)?;
            let tex = t.ok_or("no texture")?;
            let (yf, uvf) = if d.Format == DXGI_FORMAT_P010 { (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM) } else { (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM) };
            let y = srv(&self.device, &tex, yf)?;
            let uv = srv(&self.device, &tex, uvf)?;
            self.copy = Some(Copy { tex, y, uv, w: d.Width, h: d.Height, format: d.Format });
        }
        let c = self.copy.as_ref().unwrap();
        unsafe { self.ctx.CopySubresourceRegion(&c.tex, 0, 0, 0, 0, src, f.texture_index.max(0) as u32, None) };
        Ok(())
    }

    /// The output texture, `w × h` in `format` (re-made when either changes).
    fn output(&mut self, w: u32, h: u32, format: DXGI_FORMAT) -> Result<(), String> {
        if self.out.as_ref().is_some_and(|o| (o.width, o.height, o.format) == (w, h, format)) {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(err)?;
        let texture = t.ok_or("no texture")?;
        let mut v = None;
        unsafe { self.device.CreateRenderTargetView(&texture, None, Some(&mut v)) }.map_err(err)?;
        self.out = Some(Output { texture, rtv: v.ok_or("no view")?, width: w, height: h, format });
        Ok(())
    }

    /// Draws `f` upright into the output texture of `out_w × out_h` (the display size), from its `uploaded`
    /// planes when the decode thread put them on the GPU. True when the output texture was re-made (its bitmap
    /// must be too).
    pub fn convert(&mut self, f: &LvFrame, uploaded: Option<&Uploaded>, rotation: i32, out_w: u32, out_h: u32, look: &Look) -> Result<bool, String> {
        let h = f.height.max(1) as u32;
        let deep = f.bit_depth > 8;
        let (mode, alpha, nviews) = layout_of(f);
        let mut crop = [1.0f32, 1.0];
        let views: Vec<Option<ID3D11ShaderResourceView>> = if f.layout == TEXTURE {
            self.copy_texture(f)?;
            let c = self.copy.as_ref().unwrap();
            crop = [f.width.max(1) as f32 / c.w as f32, h as f32 / c.h as f32];
            vec![Some(c.y.clone()), Some(c.uv.clone())]
        } else if let Some(u) = uploaded {
            u.planes.iter().take(nviews).map(|p| Some(p.srv.clone())).collect()
        } else {
            let specs = plane_specs(f)?;
            for (i, s) in specs.iter().enumerate() {
                self.upload(i, s)?;
            }
            self.planes.iter().take(nviews).map(|p| Some(p.srv.clone())).collect()
        };
        let format = if look.white.is_some() { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
        let remade = self.out.as_ref().is_none_or(|o| (o.width, o.height, o.format) != (out_w, out_h, format));
        self.output(out_w, out_h, format)?;

        let kind = transfer_kind(look.transfer);
        let hdr_video = kind != 0;
        let depth = f.bit_depth.clamp(8, 16) as u32;
        // HDR with no matrix tag is BT.2020's.
        let matrix = if hdr_video && f.matrix == 2 { 9 } else { f.matrix };
        let rows = yuv_rows(matrix, f.full_range != 0, depth, h);
        // A 16-bit word holds code << shift: read as 0..1 that is code * 2^shift / 65535.
        let scale = if deep { 65535.0 / (((1u32 << depth) - 1) as f32 * (1u32 << f.shift.max(0)) as f32) } else { 1.0 };
        let (rot0, rot1) = turn(rotation);
        let g = gamut(look.primaries, hdr_video);
        let headroom = look.headroom.max(1.0);
        let through_linear = hdr_video || g != hdr::IDENTITY;
        let mut cb = [0f32; CB_FLOATS];
        cb[0..4].copy_from_slice(&rows[0]);
        cb[4..8].copy_from_slice(&rows[1]);
        cb[8..12].copy_from_slice(&rows[2]);
        cb[12..16].copy_from_slice(&rot0);
        cb[16..20].copy_from_slice(&rot1);
        cb[20..24].copy_from_slice(&[mode, alpha, look.white.unwrap_or(0.0), scale]);
        cb[24..28].copy_from_slice(&[crop[0], crop[1], 0.0, 0.0]);
        for (i, row) in g.iter().enumerate() {
            cb[28 + i * 4..31 + i * 4].copy_from_slice(row);
        }
        cb[40..44].copy_from_slice(&[kind as f32, headroom, KNEE * headroom, if through_linear { 1.0 } else { 0.0 }]);

        let o = self.out.as_ref().unwrap();
        unsafe {
            let ctx = &self.ctx;
            ctx.UpdateSubresource(&self.cb, 0, None, cb.as_ptr() as _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(o.rtv.clone())]), None);
            ctx.OMSetBlendState(None, None, u32::MAX);
            ctx.OMSetDepthStencilState(None, 0);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: out_w as f32, Height: out_h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.PSSetShaderResources(0, Some(&views));
            ctx.Draw(3, 0);
            // Unbound, so Direct2D can sample the output and the next upload can map the planes.
            ctx.PSSetShaderResources(0, Some(&[None, None, None, None]));
            ctx.OMSetRenderTargets(None, None);
        }
        Ok(remade)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(rows: &[[f32; 4]; 3], y: f32, u: f32, v: f32) -> [f32; 3] {
        let d = |r: &[f32; 4]| r[0] * y + r[1] * u + r[2] * v + r[3];
        [d(&rows[0]), d(&rows[1]), d(&rows[2])]
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.003)
    }

    #[test]
    fn limited_range_white_black_and_red() {
        let r = yuv_rows(1, false, 8, 1080);
        assert!(close(apply(&r, 235.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0), [1.0, 1.0, 1.0]));
        assert!(close(apply(&r, 16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0), [0.0, 0.0, 0.0]));
        // BT.709 red: Y 63, Cb 102, Cr 240.
        let red = apply(&r, 63.0 / 255.0, 102.0 / 255.0, 240.0 / 255.0);
        assert!(red[0] > 0.98 && red[1].abs() < 0.03 && red[2].abs() < 0.03, "{red:?}");
    }

    #[test]
    fn ten_bit_and_full_range() {
        let r = yuv_rows(9, false, 10, 2160);
        assert!(close(apply(&r, 940.0 / 1023.0, 512.0 / 1023.0, 512.0 / 1023.0), [1.0, 1.0, 1.0]));
        assert!(close(apply(&r, 64.0 / 1023.0, 512.0 / 1023.0, 512.0 / 1023.0), [0.0, 0.0, 0.0]));
        let f = yuv_rows(5, true, 8, 480);
        assert!(close(apply(&f, 1.0, 128.0 / 255.0, 128.0 / 255.0), [1.0, 1.0, 1.0]));
        assert!(close(apply(&f, 0.0, 128.0 / 255.0, 128.0 / 255.0), [0.0, 0.0, 0.0]));
    }

    #[test]
    fn rolloff_is_smooth_and_stays_under_the_headroom() {
        for h in [1.0f32, 2.0, 4.0, 10.0] {
            let k = KNEE * h;
            assert_eq!(tone(k * 0.5, h), k * 0.5);
            // Continuous with slope 1 at the knee.
            let d = 1e-3;
            assert!(((tone(k + d, h) - tone(k, h)) / d - 1.0).abs() < 0.01);
            let mut last = 0.0;
            for i in 0..200 {
                let v = tone(i as f32 * 0.25, h);
                assert!(v >= last && v <= h, "{h}: {v}");
                last = v;
            }
        }
        // SDR white keeps most of its brightness on an SDR display.
        assert!((tone(1.0, 1.0) - 0.908).abs() < 0.002);
    }

    #[test]
    fn hdr_streams_and_gamuts() {
        assert!(is_hdr(16) && is_hdr(18) && !is_hdr(1) && !is_hdr(2));
        assert_eq!(gamut(2, true), hdr::BT2020_TO_709);
        assert_eq!(gamut(2, false), hdr::IDENTITY);
        assert_eq!(gamut(1, true), hdr::IDENTITY);
        assert!(is_wide(1, 9) && is_wide(18, 1) && !is_wide(1, 1) && !is_wide(2, 2));
    }

    #[test]
    fn turning_maps_corners() {
        // Upright top-left (0, 0) comes from the source corner that the turn carries there.
        let at = |r: i32, u: f32, v: f32| {
            let (a, b) = turn(r);
            (a[0] * u + a[1] * v + a[2], b[0] * u + b[1] * v + b[2])
        };
        assert_eq!(at(0, 0.0, 0.0), (0.0, 0.0));
        assert_eq!(at(90, 0.0, 0.0), (0.0, 1.0)); // turned clockwise: the source's bottom-left is now top-left
        assert_eq!(at(180, 0.0, 0.0), (1.0, 1.0));
        assert_eq!(at(270, 0.0, 0.0), (1.0, 0.0));
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::*;
    use windows::Win32::Foundation::HMODULE;

    fn warp() -> ID3D11Device {
        let kind = if std::env::var_os("LOOKER_TEST_HW").is_some() { D3D_DRIVER_TYPE_HARDWARE } else { D3D_DRIVER_TYPE_WARP };
        let mut d = None;
        unsafe {
            D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d), None, None).unwrap();
        }
        d.unwrap()
    }

    fn read_pixel(dev: &ID3D11Device, tex: &ID3D11Texture2D) -> [u8; 4] {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut st = None;
            dev.CreateTexture2D(&desc, None, Some(&mut st)).unwrap();
            let st = st.unwrap();
            let ctx = dev.GetImmediateContext().unwrap();
            ctx.CopyResource(&st, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let p = std::slice::from_raw_parts(m.pData as *const u8, 4);
            let out = [p[0], p[1], p[2], p[3]];
            ctx.Unmap(&st, 0);
            out
        }
    }

    /// A 12-bit 4:4:4 picture with half alpha (ProRes 4444's layout): white at half alpha, premultiplied, is 50 %
    /// grey with alpha 128 in BGRA.
    #[test]
    fn planar_alpha_is_premultiplied() {
        let dev = warp();
        let mut conv = Converter::new(&dev).unwrap();
        let (w, h) = (4usize, 4usize);
        let y = vec![3760u16; w * h]; // 12-bit limited-range white (235 << 4)
        let c = vec![2048u16; w * h];
        let a = vec![2048u16; w * h];
        let mut f: LvFrame = unsafe { std::mem::zeroed() };
        f.kind = crate::imaging::video::VIDEO;
        f.layout = PLANAR;
        f.width = w as i32;
        f.height = h as i32;
        f.bit_depth = 12;
        f.nplanes = 4;
        f.matrix = 1;
        f.data = [y.as_ptr() as _, c.as_ptr() as _, c.as_ptr() as _, a.as_ptr() as _];
        f.linesize = [(w * 2) as i32; 4];
        conv.convert(&f, None, 0, w as u32, h as u32, &SDR).unwrap();
        let px = read_pixel(&dev, &conv.out.as_ref().unwrap().texture);
        println!("BGRA {px:?}");
        assert!((px[3] as i32 - 128).abs() <= 2 && (px[0] as i32 - 128).abs() <= 3, "{px:?}");
        // The same through the decode thread's upload.
        let up = Uploader::new(&dev).unwrap().upload(&f).unwrap();
        let mut conv = Converter::new(&dev).unwrap();
        conv.convert(&f, Some(&up), 0, w as u32, h as u32, &SDR).unwrap();
        assert_eq!(read_pixel(&dev, &conv.out.as_ref().unwrap().texture), px);
    }

    const SDR: Look = Look { white: None, headroom: 1.0, transfer: 1, primaries: 1 };

    /// A grey 10-bit limited-range 4:2:0 picture whose signal is `e` (0..1).
    fn grey10(e: f32) -> (Vec<u16>, Vec<u16>, LvFrame) {
        let (w, h) = (4usize, 4usize);
        let y = vec![(64.0 + e * 876.0).round() as u16; w * h];
        let c = vec![512u16; w * h / 4];
        let mut f: LvFrame = unsafe { std::mem::zeroed() };
        f.kind = crate::imaging::video::VIDEO;
        f.layout = PLANAR;
        f.width = w as i32;
        f.height = h as i32;
        f.bit_depth = 10;
        f.nplanes = 3;
        f.chroma_w_shift = 1;
        f.chroma_h_shift = 1;
        f.matrix = 9;
        f.data = [y.as_ptr() as _, c.as_ptr() as _, c.as_ptr() as _, std::ptr::null()];
        f.linesize = [(w * 2) as i32, w as i32, w as i32, 0];
        (y, c, f)
    }

    fn read_half(dev: &ID3D11Device, tex: &ID3D11Texture2D) -> [f32; 4] {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut st = None;
            dev.CreateTexture2D(&desc, None, Some(&mut st)).unwrap();
            let st = st.unwrap();
            let ctx = dev.GetImmediateContext().unwrap();
            ctx.CopyResource(&st, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let p = std::slice::from_raw_parts(m.pData as *const u16, 4);
            let half = |b: u16| {
                let (s, e, f) = ((b >> 15) as i32, ((b >> 10) & 31) as i32, (b & 1023) as f32);
                let v = if e == 0 { f / 1024.0 * 2f32.powi(-14) } else { (1.0 + f / 1024.0) * 2f32.powi(e - 15) };
                if s == 1 { -v } else { v }
            };
            let out = [half(p[0]), half(p[1]), half(p[2]), half(p[3])];
            ctx.Unmap(&st, 0);
            out
        }
    }

    /// PQ's reference white (203 nits) is SDR white on an HDR display with room above it, and rolled off a little
    /// below SDR white on an SDR one; 1000 nits fits a 4x headroom only rolled off.
    #[test]
    fn pq_reference_white() {
        let dev = warp();
        let mut conv = Converter::new(&dev).unwrap();
        let (_y, _c, f) = grey10(0.5807);
        let hdr10 = Look { white: Some(2.5), headroom: 4.0, transfer: 16, primaries: 9 };
        conv.convert(&f, None, 0, 4, 4, &hdr10).unwrap();
        let px = read_half(&dev, &conv.out.as_ref().unwrap().texture);
        println!("scRGB {px:?}");
        assert!(px[..3].iter().all(|v| (v - 2.5).abs() < 0.05), "{px:?}");
        conv.convert(&f, None, 0, 4, 4, &Look { white: None, headroom: 1.0, ..hdr10 }).unwrap();
        let px = read_pixel(&dev, &conv.out.as_ref().unwrap().texture);
        let want = (crate::colour::srgb_encode(tone(1.0, 1.0)) * 255.0).round() as i32;
        println!("BGRA {px:?}, want {want}");
        assert!(px[..3].iter().all(|&v| (v as i32 - want).abs() <= 2), "{px:?}");
        // 1000 nits (PQ 0.7518): 4.93 SDR whites into a headroom of 4.
        let (_y, _c, f) = grey10(0.7518);
        conv.convert(&f, None, 0, 4, 4, &Look { white: Some(1.0), ..hdr10 }).unwrap();
        let px = read_half(&dev, &conv.out.as_ref().unwrap().texture);
        let want = tone(1000.0 / 203.0, 4.0);
        println!("1000 nits -> {px:?}, want {want}");
        assert!(px[..3].iter().all(|v| (v - want).abs() < 0.05) && want < 4.0 && want > 3.0, "{px:?}");
    }

    /// HLG's 75 % is reference white, as PQ's 203 nits is.
    #[test]
    fn hlg_reference_white() {
        let dev = warp();
        let mut conv = Converter::new(&dev).unwrap();
        let (_y, _c, f) = grey10(0.75);
        conv.convert(&f, None, 0, 4, 4, &Look { white: Some(1.0), headroom: 4.0, transfer: 18, primaries: 9 }).unwrap();
        let px = read_half(&dev, &conv.out.as_ref().unwrap().texture);
        println!("scRGB {px:?}");
        assert!(px[..3].iter().all(|v| (v - 1.0).abs() < 0.04), "{px:?}");
    }
}
