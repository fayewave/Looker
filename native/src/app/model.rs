//! The live view of a 3D model. The decode pool's still (model/render.rs) shows first, like any image; the
//! mesh loads on its own thread meanwhile (usually from the still's parse, kept in `model::load`), and once it
//! is in, the viewport draws the model on the window's own device instead: drag to orbit, right- or Shift+drag
//! to pan, the wheel (or the zoom buttons) to move in and out, double-click or Fit to go back to the start.

use super::*;
use crate::model::render::{Camera, GpuMesh, Output, Renderer, Target};
use crate::model::Mesh;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT};

pub(super) const WM_MODEL_LOADED: u32 = WM_APP + 13;

pub(super) struct ModelView {
    pub path: PathBuf,
    mesh: Option<Arc<Mesh>>,
    camera: Camera,
    /// On the current device; dropped when it changes.
    gpu: Option<(Renderer, GpuMesh)>,
    target: Option<Target>,
    bitmap: Option<ID2D1Bitmap1>,
    /// What `bitmap` shows: re-rendered when any of it changes.
    drawn: Option<(Camera, Output, u32, u32)>,
}

impl ModelView {
    /// Bitmaps die with their device (WARP to hardware): so does everything of the model's on it.
    pub(super) fn device_lost(&mut self) {
        self.gpu = None;
        self.target = None;
        self.bitmap = None;
        self.drawn = None;
    }
}

/// 12345 -> "12,345".
fn thousands(v: usize) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(super) struct Loaded {
    path: PathBuf,
    mesh: Result<Arc<Mesh>, String>,
}

impl App {
    /// After every navigation: a model on screen starts loading its mesh; anything else drops the last one.
    pub(super) fn sync_model(&mut self) {
        let want = self.current_path().filter(|p| format::from_extension(p.extension().and_then(|e| e.to_str())) == format::Format::Model).map(Path::to_path_buf);
        let Some(path) = want else {
            self.model = None;
            return;
        };
        if self.model.as_ref().is_some_and(|m| m.path == path) {
            return;
        }
        self.model = Some(ModelView { path: path.clone(), mesh: None, camera: Camera::default(), gpu: None, target: None, bitmap: None, drawn: None });
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("model".into())
            .spawn(move || {
                let mesh = unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    CoCreateInstance(&windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER)
                }
                .map_err(|e| e.message().to_string())
                .and_then(|f| crate::model::load(&f, &path));
                let ptr = Box::into_raw(Box::new(Loaded { path, mesh }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), WM_MODEL_LOADED, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    pub(super) fn on_model_loaded(&mut self, l: Loaded) {
        let Some(m) = self.model.as_mut().filter(|m| m.path == l.path) else { return };
        match l.mesh {
            Ok(mesh) => m.mesh = Some(mesh),
            // The still (or the decode's error) stays on screen.
            Err(e) => crate::trace::mark(format!("model view: {e}")),
        }
        self.invalidate();
    }

    /// The model is on screen to orbit: its mesh is in and no other file covers it.
    pub(super) fn model_active(&self) -> bool {
        self.placeholder.is_none() && self.model.as_ref().is_some_and(|m| m.mesh.is_some())
    }

    /// Before the frame begins (Direct2D isn't mid-draw): renders the model when its view changed.
    pub(super) fn prepare_model(&mut self, g: &Gfx) {
        if !self.model_active() {
            return;
        }
        let a = self.image_area();
        let s = self.scale();
        let (w, h) = (((a.right - a.left) * s).round().max(1.0) as u32, ((a.bottom - a.top) * s).round().max(1.0) as u32);
        let (format, out) = if g.space().is_scrgb() {
            (DXGI_FORMAT_R16G16B16A16_FLOAT, Output::Linear(g.space().white_scale()))
        } else {
            (DXGI_FORMAT_B8G8R8A8_UNORM, Output::Srgb)
        };
        let Some(m) = self.model.as_mut() else { return };
        let Some(mesh) = m.mesh.clone() else { return };
        if m.drawn == Some((m.camera, out, w, h)) && m.bitmap.is_some() {
            return;
        }
        let r = (|| -> Result<(), String> {
            if m.gpu.is_none() {
                let r = Renderer::new(g.d3d())?;
                let gpu = r.upload(&mesh)?;
                m.gpu = Some((r, gpu));
            }
            let (r, gpu) = m.gpu.as_ref().unwrap();
            if m.target.as_ref().is_none_or(|t| (t.width, t.height, t.format) != (w, h, format)) {
                m.bitmap = None;
                m.target = Some(r.target(w, h, format)?);
            }
            let t = m.target.as_ref().unwrap();
            r.draw(&mesh, gpu, t, &m.camera, out);
            if m.bitmap.is_none() {
                m.bitmap = Some(g.texture_bitmap(&t.texture, format).map_err(|e| e.message().to_string())?);
            }
            m.drawn = Some((m.camera, out, w, h));
            Ok(())
        })();
        if let Err(e) = r {
            crate::trace::mark(format!("model view failed: {e}"));
            m.mesh = None; // back to the still
        }
    }

    /// Draws the model into the image area; false when there is none to draw (the still shows instead).
    pub(super) fn draw_model(&self, g: &Gfx) -> bool {
        let Some(b) = self.model.as_ref().filter(|_| self.model_active()).and_then(|m| m.bitmap.as_ref()) else { return false };
        g.draw_bitmap(b, self.image_area(), 1.0);
        true
    }

    /// A drag across the viewport of `dx`, `dy` DIPs: orbits, or with `pan` slides the model along.
    pub(super) fn model_drag(&mut self, dx: f32, dy: f32, pan: bool) {
        let a = self.image_area();
        let Some(m) = self.model.as_mut() else { return };
        if pan {
            if let Some(mesh) = &m.mesh {
                m.camera.pan_by(mesh, dx, dy, a.right - a.left, a.bottom - a.top);
            }
        } else {
            m.camera.orbit(dx, dy);
        }
        self.invalidate();
    }

    /// Moves in (`factor` < 1) or out.
    pub(super) fn model_zoom(&mut self, factor: f32) {
        if let Some(m) = self.model.as_mut() {
            m.camera.dolly(factor);
            self.invalidate();
        }
    }

    pub(super) fn model_reset(&mut self) {
        if let Some(m) = self.model.as_mut() {
            m.camera = Camera::default();
            self.invalidate();
        }
    }

    /// The info card's rows about the model, once it has loaded.
    pub(super) fn model_facts(&self) -> Vec<(&'static str, String)> {
        let Some(mesh) = self.model.as_ref().and_then(|m| m.mesh.as_ref()) else { return Vec::new() };
        let size: Vec<String> = (0..3).map(|i| format!("{}", ((mesh.max[i] - mesh.min[i]) * 1000.0).round() / 1000.0)).collect();
        let textured = mesh.used_materials().filter(|m| m.map_kd.is_some()).count();
        let materials = mesh.used_materials().count();
        vec![
            ("Triangles", thousands(mesh.triangles())),
            ("Vertices", thousands(mesh.vertices.len())),
            ("Materials", if textured > 0 { format!("{materials} ({textured} textured)") } else { materials.to_string() }),
            ("Bounds", format!("{} × {} × {} units", size[0], size[1], size[2])),
        ]
    }

    /// The status row's facts about the model, once it has loaded.
    pub(super) fn model_status(&self) -> Option<String> {
        let mesh = self.model.as_ref()?.mesh.as_ref()?;
        let n = thousands;
        let tris = mesh.triangles();
        let mats = mesh.used_materials().count();
        Some(format!(
            "{} {}   ·   {} {}",
            n(tris),
            if tris == 1 { "triangle" } else { "triangles" },
            n(mats),
            if mats == 1 { "material" } else { "materials" }
        ))
    }
}
