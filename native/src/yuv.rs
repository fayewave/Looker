//! A decoded video picture to an upright RGB texture Direct2D draws: one pixel shader for every layout the
//! decoder hands out (looker_video.c's LvFrame). A GPU picture (a slice of the decoder's NV12/P010 texture array)
//! is copied into a texture of ours first (the decoder's can't be sampled); CPU planes are uploaded. The shader
//! applies the stream's YUV matrix and range, turns the picture upright (the container's rotation), premultiplies
//! alpha, and writes either 8-bit sRGB-encoded values (an SDR swap chain) or linear scRGB at SDR white's level
//! (FP16, the HDR / Auto Color Management one), like the 3D view (model/render.rs).

use std::sync::OnceLock;

use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, PCSTR, s};

use crate::imaging::video::{LvFrame, PLANAR, RGBA, TEXTURE};

const SHADER: &str = r#"
cbuffer C : register(b0) {
    float4 r0, r1, r2;  // rgb = (r0, r1, r2) . (y, u, v, 1)
    float4 rot0, rot1;  // source uv = (rot0, rot1) . (u, v, 1): the turn to upright
    float4 opts;        // x: 0 three planes, 1 Y + interleaved UV, 2 RGBA; y: alpha plane; z: linear output at this white (0 = encoded); w: sample scale
    float4 crop;        // xy: the picture's share of the texture
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
    if (opts.z > 0) c.rgb = float3(lin(c.r), lin(c.g), lin(c.b)) * opts.z;
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

struct Plane {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
}

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
    planes: Vec<Plane>,
    copy: Option<Copy>,
    pub out: Option<Output>,
}

impl Converter {
    pub fn new(device: &ID3D11Device) -> Result<Converter, String> {
        let (vs_code, ps_code) = bytecode()?;
        unsafe {
            let ctx = device.GetImmediateContext().map_err(err)?;
            let mut vs = None;
            device.CreateVertexShader(vs_code, None, Some(&mut vs)).map_err(err)?;
            let mut ps = None;
            device.CreatePixelShader(ps_code, None, Some(&mut ps)).map_err(err)?;
            let desc = D3D11_BUFFER_DESC { ByteWidth: 112, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
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

    fn srv(&self, tex: &ID3D11Texture2D, format: DXGI_FORMAT) -> Result<ID3D11ShaderResourceView, String> {
        let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
        };
        let mut v = None;
        unsafe { self.device.CreateShaderResourceView(tex, Some(&desc), Some(&mut v)) }.map_err(err)?;
        v.ok_or_else(|| "no view".into())
    }

    /// A CPU plane of `w × h` samples in `format`, uploaded into plane slot `i` (a dynamic texture, re-made only
    /// when its size or format changes).
    fn upload(&mut self, i: usize, w: u32, h: u32, format: DXGI_FORMAT, data: *const u8, stride: usize, row_bytes: usize) -> Result<(), String> {
        if self.planes.get(i).is_none_or(|p| (p.w, p.h, p.format) != (w, h, format)) {
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
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(err)?;
            let tex = t.ok_or("no texture")?;
            let srv = self.srv(&tex, format)?;
            let plane = Plane { tex, srv, w, h, format };
            if i < self.planes.len() {
                self.planes[i] = plane;
            } else {
                self.planes.push(plane);
            }
        }
        let p = &self.planes[i];
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&p.tex, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).map_err(err)?;
            let dst = m.pData as *mut u8;
            let pitch = m.RowPitch as usize;
            for y in 0..h as usize {
                std::ptr::copy_nonoverlapping(data.add(y * stride), dst.add(y * pitch), row_bytes.min(pitch));
            }
            self.ctx.Unmap(&p.tex, 0);
        }
        Ok(())
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
            let y = self.srv(&tex, yf)?;
            let uv = self.srv(&tex, uvf)?;
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

    /// Draws `f` upright into the output texture of `out_w × out_h` (the display size): 8-bit encoded when
    /// `white` is None, else FP16 linear at that SDR white level. True when the output texture was re-made (its
    /// bitmap must be too).
    pub fn convert(&mut self, f: &LvFrame, rotation: i32, out_w: u32, out_h: u32, white: Option<f32>) -> Result<bool, String> {
        let (w, h) = (f.width.max(1) as u32, f.height.max(1) as u32);
        let deep = f.bit_depth > 8;
        let (mode, crop, nviews) = match f.layout {
            TEXTURE => {
                self.copy_texture(f)?;
                let c = self.copy.as_ref().unwrap();
                (1.0, [w as f32 / c.w as f32, h as f32 / c.h as f32], 2)
            }
            PLANAR => {
                let (cw, ch) = ((w + (1 << f.chroma_w_shift) - 1) >> f.chroma_w_shift, (h + (1 << f.chroma_h_shift) - 1) >> f.chroma_h_shift);
                let one = if deep { DXGI_FORMAT_R16_UNORM } else { DXGI_FORMAT_R8_UNORM };
                let two = if deep { DXGI_FORMAT_R16G16_UNORM } else { DXGI_FORMAT_R8G8_UNORM };
                let bytes = if deep { 2 } else { 1 };
                self.upload(0, w, h, one, f.data[0], f.linesize[0] as usize, w as usize * bytes)?;
                if f.nplanes == 2 {
                    self.upload(1, cw, ch, two, f.data[1], f.linesize[1] as usize, cw as usize * bytes * 2)?;
                } else {
                    self.upload(1, cw, ch, one, f.data[1], f.linesize[1] as usize, cw as usize * bytes)?;
                    self.upload(2, cw, ch, one, f.data[2], f.linesize[2] as usize, cw as usize * bytes)?;
                    if f.nplanes == 4 {
                        self.upload(3, w, h, one, f.data[3], f.linesize[3] as usize, w as usize * bytes)?;
                    }
                }
                (if f.nplanes == 2 { 1.0 } else { 0.0 }, [1.0, 1.0], f.nplanes.max(2) as usize)
            }
            RGBA => {
                self.upload(0, w, h, DXGI_FORMAT_R8G8B8A8_UNORM, f.data[0], f.linesize[0] as usize, w as usize * 4)?;
                (2.0, [1.0, 1.0], 1)
            }
            l => return Err(format!("unknown picture layout {l}")),
        };
        let format = if white.is_some() { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
        let remade = self.out.as_ref().is_none_or(|o| (o.width, o.height, o.format) != (out_w, out_h, format));
        self.output(out_w, out_h, format)?;

        let depth = f.bit_depth.clamp(8, 16) as u32;
        let rows = yuv_rows(f.matrix, f.full_range != 0, depth, h);
        // A 16-bit word holds code << shift: read as 0..1 that is code * 2^shift / 65535.
        let scale = if deep { 65535.0 / (((1u32 << depth) - 1) as f32 * (1u32 << f.shift.max(0)) as f32) } else { 1.0 };
        let (rot0, rot1) = turn(rotation);
        let mut cb = [0f32; 28];
        cb[0..4].copy_from_slice(&rows[0]);
        cb[4..8].copy_from_slice(&rows[1]);
        cb[8..12].copy_from_slice(&rows[2]);
        cb[12..16].copy_from_slice(&rot0);
        cb[16..20].copy_from_slice(&rot1);
        cb[20..24].copy_from_slice(&[mode, if f.layout == PLANAR && f.nplanes == 4 { 1.0 } else { 0.0 }, white.unwrap_or(0.0), scale]);
        cb[24..28].copy_from_slice(&[crop[0], crop[1], 0.0, 0.0]);

        let views: Vec<Option<ID3D11ShaderResourceView>> = if f.layout == TEXTURE {
            let c = self.copy.as_ref().unwrap();
            vec![Some(c.y.clone()), Some(c.uv.clone())]
        } else {
            self.planes.iter().take(nviews).map(|p| Some(p.srv.clone())).collect()
        };
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
        conv.convert(&f, 0, w as u32, h as u32, None).unwrap();
        let px = read_pixel(&dev, &conv.out.as_ref().unwrap().texture);
        println!("BGRA {px:?}");
        assert!((px[3] as i32 - 128).abs() <= 2 && (px[0] as i32 - 128).abs() <= 3, "{px:?}");
    }
}
