//! The live view of a 3D model. The decode pool's still (model/render.rs) shows first, like any image; the
//! mesh loads on its own thread meanwhile (usually from the still's parse, kept in `model::load`), and once it
//! is in, the viewport draws the model on the window's own device instead: drag to orbit, right- or Shift+drag
//! to pan, the wheel (or the zoom buttons) to move in and out, double-click or Fit to go back to the start.
//!
//! Holding the right button (to pan) shows the orbit centre as a 3D cross inside the model, fading out once let
//! go. A view cube in the top right turns with the model; a click on one of its faces turns the model to look at
//! that side, a drag on it orbits (Faye, 2026-11-10, after Blender's).

use std::time::{Duration, Instant};

use super::*;
use crate::model::render::{Camera, GpuMesh, Output, Pivot, Place, Renderer, Target};
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
    /// What the last render was of (the theme's darkness too: the floor's lines follow it).
    drawn: Option<(Camera, Output, u32, u32, Place, Option<Pivot>, bool)>,
    /// The orbit centre is being moved (the cross shows), or when it was let go (the cross fades).
    panning: bool,
    pan_released: Option<Instant>,
    /// When the right button went down on the model (held still, the cross shows without moving it).
    pan_pressed: Option<Instant>,
    /// A view cube click turning the model: from where, to which yaw and pitch, since when.
    snap: Option<(Camera, (f32, f32), Instant)>,
}

/// How long the pivot cross takes to fade once let go, and a view cube turn.
const PIVOT_FADE: Duration = Duration::from_millis(450);
const SNAP: Duration = Duration::from_millis(260);
/// The view cube: its square's side, how far its centre is from a face (DIPs), its margin from the corner.
const CUBE_BOX: f32 = 96.0;
const CUBE_HALF: f32 = 25.0;
const CUBE_MARGIN: f32 = 14.0;

/// The cube's faces: the side of the model each looks at, its label, and which ways are right and down on it seen
/// head-on (as the camera sees it after a click).
const FACES: [([f32; 3], &str, [f32; 3], [f32; 3]); 6] = [
    ([0.0, 0.0, 1.0], "Front", [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
    ([0.0, 0.0, -1.0], "Back", [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
    ([1.0, 0.0, 0.0], "Right", [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
    ([-1.0, 0.0, 0.0], "Left", [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
    ([0.0, 1.0, 0.0], "Top", [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, -1.0, 0.0], "Bottom", [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
];

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Whether (x, y) is inside the convex quad `q` (either winding).
fn in_quad(q: &[(f32, f32); 4], x: f32, y: f32) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        if c != 0.0 {
            if sign != 0.0 && c.signum() != sign {
                return false;
            }
            sign = c.signum();
        }
    }
    true
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
        self.model = Some(ModelView {
            path: path.clone(),
            mesh: None,
            camera: Camera::default(),
            gpu: None,
            target: None,
            bitmap: None,
            drawn: None,
            panning: false,
            pan_released: None,
            pan_pressed: None,
            snap: None,
        });
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
        // The whole photo layer, framed to the area between the panels (as a photo is fitted there).
        let layer = self.photo_layer();
        let a = self.image_area();
        let s = self.scale();
        let (w, h) = (((layer.right - layer.left) * s).round().max(1.0) as u32, ((layer.bottom - layer.top) * s).round().max(1.0) as u32);
        let place = Place::inside([a.left, a.top, a.right, a.bottom], [layer.left, layer.top, layer.right, layer.bottom]);
        let (format, out) = if g.space().is_scrgb() {
            (DXGI_FORMAT_R16G16B16A16_FLOAT, Output::Linear(g.space().white_scale()))
        } else {
            (DXGI_FORMAT_B8G8R8A8_UNORM, Output::Srgb)
        };
        let dip = s;
        let Some(m) = self.model.as_mut() else { return };
        let Some(mesh) = m.mesh.clone() else { return };
        // A view cube turn in progress.
        if let Some((from, (yaw, pitch), t0)) = m.snap {
            let t: f32 = (t0.elapsed().as_secs_f32() / SNAP.as_secs_f32()).min(1.0);
            let k = 1.0 - (1.0 - t).powi(4);
            let mut dy = yaw - from.yaw;
            dy = (dy + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            m.camera.yaw = from.yaw + dy * k;
            m.camera.pitch = from.pitch + (pitch - from.pitch) * k;
            if t >= 1.0 {
                m.snap = None;
            }
        }
        let alpha = if m.panning {
            1.0
        } else {
            m.pan_released.map_or(0.0, |t| 1.0 - (t.elapsed().as_secs_f32() / PIVOT_FADE.as_secs_f32()).min(1.0))
        };
        if alpha <= 0.0 {
            m.pan_released = None;
        }
        let pivot = (alpha > 0.0).then_some(Pivot { alpha: alpha * alpha * (3.0 - 2.0 * alpha), dip });
        let dark = crate::theme::t().dark;
        if m.drawn == Some((m.camera, out, w, h, place, pivot, dark)) && m.bitmap.is_some() {
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
            r.draw(&mesh, gpu, t, &m.camera, &place, out, pivot, true);
            if m.bitmap.is_none() {
                m.bitmap = Some(g.texture_bitmap(&t.texture, format).map_err(|e| e.message().to_string())?);
            }
            m.drawn = Some((m.camera, out, w, h, place, pivot, dark));
            Ok(())
        })();
        if let Err(e) = r {
            crate::trace::mark(format!("model view failed: {e}"));
            m.mesh = None; // back to the still
        }
    }

    /// Draws the model over the photo layer; false when there is none to draw (the still shows instead).
    pub(super) fn draw_model(&self, g: &Gfx) -> bool {
        let Some(b) = self.model.as_ref().filter(|_| self.model_active()).and_then(|m| m.bitmap.as_ref()) else { return false };
        g.draw_bitmap(b, self.photo_layer(), 1.0);
        true
    }

    /// A drag across the viewport of `dx`, `dy` DIPs: orbits, or with `pan` slides the model along.
    pub(super) fn model_drag(&mut self, dx: f32, dy: f32, pan: bool) {
        let a = self.image_area();
        let Some(m) = self.model.as_mut() else { return };
        if pan {
            if let Some(mesh) = &m.mesh {
                m.camera.pan_by(mesh, dx, dy, a.right - a.left, a.bottom - a.top);
                m.panning = true;
                m.pan_released = None;
            }
        } else {
            m.snap = None; // a drag takes over from a view cube turn
            m.camera.orbit(dx, dy);
        }
        self.invalidate();
    }

    /// The right button went down on the model: the pivot cross shows at once, so holding it still shows where
    /// the orbit centre is (Faye, 2026-11-10).
    pub(super) fn model_pan_start(&mut self) {
        if let Some(m) = self.model.as_mut().filter(|m| m.mesh.is_some()) {
            m.panning = true;
            m.pan_released = None;
            m.pan_pressed = Some(Instant::now());
            self.invalidate();
        }
    }

    /// The pan let go: the pivot cross fades out. True when the button was held long enough to look at the cross
    /// (no context menu then, as after a pan).
    pub(super) fn model_pan_end(&mut self) -> bool {
        let Some(m) = self.model.as_mut().filter(|m| m.panning) else { return false };
        m.panning = false;
        m.pan_released = Some(Instant::now());
        let held = m.pan_pressed.take().is_some_and(|t| t.elapsed() >= Duration::from_millis(350));
        self.invalidate();
        held
    }

    /// The pivot cross is fading, or the view cube is turning the model: draw again next vsync.
    pub(super) fn model_animating(&self) -> bool {
        self.model.as_ref().is_some_and(|m| m.pan_released.is_some() || m.snap.is_some())
    }

    fn cube_rect(&self) -> D2D_RECT_F {
        let a = self.image_area();
        rect(a.right - CUBE_MARGIN - CUBE_BOX, a.top + CUBE_MARGIN, CUBE_BOX, CUBE_BOX)
    }

    /// The view cube's faces that face the camera, back to front: (face, its corners on screen, how squarely it
    /// faces the camera). The corners run top-left, top-right, bottom-right, bottom-left of its label.
    fn cube_faces(&self, cam: &Camera) -> Vec<(usize, [(f32, f32); 4], f32)> {
        let (back, right, up) = cam.axes();
        let r = self.cube_rect();
        let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
        let proj = |p: [f32; 3]| (cx + dot3(p, right) * CUBE_HALF, cy - dot3(p, up) * CUBE_HALF);
        let mut out = Vec::new();
        for (i, (n, _, rt, dn)) in FACES.iter().enumerate() {
            let facing = dot3(*n, back);
            if facing <= 0.02 {
                continue;
            }
            let at = |sr: f32, sd: f32| proj(std::array::from_fn(|k| n[k] + rt[k] * sr + dn[k] * sd));
            out.push((i, [at(-1.0, -1.0), at(1.0, -1.0), at(1.0, 1.0), at(-1.0, 1.0)], facing));
        }
        out.sort_by(|a, b| a.2.total_cmp(&b.2));
        out
    }

    /// The view cube over the model's top right corner, turned as the model is.
    pub(super) fn draw_view_cube(&mut self, g: &Gfx) {
        if !self.model_active() {
            return;
        }
        let Some(cam) = self.model.as_ref().map(|m| m.camera) else { return };
        let box_r = self.cube_rect();
        self.hits.add(Hit::ModelCube, box_r);
        let faces = self.cube_faces(&cam);
        let over = (self.hover == Some(Hit::ModelCube)).then(|| faces.iter().rev().find(|f| in_quad(&f.1, self.mouse.0, self.mouse.1)).map(|f| f.0)).flatten();
        let raised = theme::raised();
        let t = theme::t();
        for (i, q, facing) in &faces {
            let pts: Vec<(f32, f32)> = q.to_vec();
            // Neutral greys, lighter the more squarely a face looks at you; the one under the pointer in the accent.
            let fill = if Some(*i) == over { theme::mix(raised.popup, t.accent, 0.55) } else { theme::mix(raised.popup, raised.text, 0.06 + 0.16 * facing) };
            // A little see-through (Faye, 2026-11-10): the model shows behind it.
            g.fill_polygon(&pts, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.72, ..fill });
            let mut ring = pts.clone();
            ring.push(pts[0]);
            g.polyline(&ring, rgb(raised.divider), 1.0);
            if *facing > 0.3 {
                // The label drawn in a face-sized square mapped onto the face.
                let side = 2.0 * CUBE_HALF;
                let (o, a, b) = (q[0], q[1], q[3]);
                let m = windows_numerics::Matrix3x2 {
                    M11: (a.0 - o.0) / side,
                    M12: (a.1 - o.1) / side,
                    M21: (b.0 - o.0) / side,
                    M22: (b.1 - o.1) / side,
                    M31: o.0,
                    M32: o.1,
                };
                let ink = gfx::rgba(if Some(*i) == over { 0xFFFFFF } else { raised.text }, (0.4 + 0.6 * facing).min(1.0));
                g.with_transform(&m, || g.text(&wide(FACES[*i].1), &g.fonts.caption, rect(0.0, 0.0, side, side), ink, Align::Center));
            }
        }
    }

    /// A click on the view cube: the model turns to look at the face under the pointer.
    pub(super) fn view_cube_click(&mut self, x: f32, y: f32) {
        let Some(cam) = self.model.as_ref().map(|m| m.camera) else { return };
        let faces = self.cube_faces(&cam);
        let Some(&(i, _, _)) = faces.iter().rev().find(|f| in_quad(&f.1, x, y)) else { return };
        if let Some(m) = self.model.as_mut() {
            m.snap = Some((m.camera, m.camera.facing(FACES[i].0), Instant::now()));
            self.invalidate();
        }
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
