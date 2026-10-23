use std::collections::{HashMap, HashSet};

use windows::core::Result;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::IDXGIOutput;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{GetCurrentProcessId, Sleep};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::WindowCapture;
use crate::overlay::{self, OverlayWindow};
use crate::physics::{JelloBody, Rect};
use crate::renderer::{self, Renderer, Wallpaper};
use crate::SETTINGS;

struct WindowInfo {
    hwnd: HWND,
    rect: Rect,
    /// Position in the global z-order, 0 = frontmost.
    z: usize,
}

fn key(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

pub struct WindowManager {
    renderer: Renderer,
    dcomp: IDCompositionDevice,
    overlays: Vec<OverlayWindow>,
    /// (SDR white, HDR boost) per overlay, re-read every couple of seconds.
    brightness: Vec<(f32, f32)>,
    last_brightness_check: f64,
    wallpaper: Option<Wallpaper>,
    vblank_output: Option<IDXGIOutput>,

    bodies: HashMap<isize, JelloBody>,
    captures: HashMap<isize, WindowCapture>,
    /// Windows we couldn't capture, with the time we may retry them.
    retry_after: HashMap<isize, f64>,

    last_tick: f64,
    was_mouse_down: bool,
    first_frame: bool,
    needs_redraw: bool,
    last_frame_signature: u64,
    last_window_ids: Vec<isize>,
    my_pid: u32,
    qpc_freq: f64,
}

// Every capture holds two full-size frame buffers plus our copy, so this bounds memory use.
const MAX_CAPTURES: usize = 16;
const MAX_NEW_CAPTURES_PER_TICK: usize = 4;
// Frontmost windows update at full rate; ones further back at a lower rate.
const FULL_RATE_WINDOWS: usize = 6;
const FULL_FPS: u32 = 60;
const BACKGROUND_FPS: u32 = 15;

impl WindowManager {
    pub fn new() -> Result<Self> {
        let renderer = Renderer::new()?;
        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&renderer.dxgi_device)? };
        let vblank_output = unsafe { renderer.dxgi_device.GetAdapter()?.EnumOutputs(0).ok() };
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq)? };
        let mut m = Self {
            renderer,
            dcomp,
            overlays: Vec::new(),
            brightness: Vec::new(),
            last_brightness_check: 0.0,
            wallpaper: None,
            vblank_output,
            bodies: HashMap::new(),
            captures: HashMap::new(),
            retry_after: HashMap::new(),
            last_tick: 0.0,
            was_mouse_down: false,
            first_frame: true,
            needs_redraw: true,
            last_frame_signature: 0,
            last_window_ids: Vec::new(),
            my_pid: unsafe { GetCurrentProcessId() },
            qpc_freq: freq as f64,
        };
        m.build_overlays()?;
        Ok(m)
    }

    fn build_overlays(&mut self) -> Result<()> {
        self.overlays.clear();
        for mon in overlay::monitors() {
            self.overlays.push(OverlayWindow::new(mon, &self.renderer, &self.dcomp)?);
        }
        self.brightness = self.overlays.iter().map(|o| o.brightness()).collect();
        self.last_brightness_check = self.now();
        self.wallpaper = self.renderer.load_wallpaper();
        self.needs_redraw = true;
        Ok(())
    }

    fn now(&self) -> f64 {
        let mut t = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut t);
        }
        t as f64 / self.qpc_freq
    }

    /// Blocks until the next vertical blank so the loop runs at display rate.
    pub fn wait_for_vblank(&self) {
        let waited = self.vblank_output.as_ref().is_some_and(|o| unsafe { o.WaitForVBlank().is_ok() });
        if !waited {
            unsafe { Sleep(8) };
        }
    }

    // MARK: - Frame loop

    pub fn tick(&mut self) -> Result<()> {
        if SETTINGS.with(|s| s.rebuild.take()) {
            self.build_overlays()?;
        }
        if SETTINGS.with(|s| s.dirty.take()) {
            self.needs_redraw = true;
        }
        let (hdr_enabled, jello_enabled, triangles_enabled) =
            SETTINGS.with(|s| (s.hdr.get(), s.jello.get(), s.triangles.get()));

        let now = self.now();
        let dt = (now - self.last_tick).clamp(1.0 / 240.0, 1.0 / 30.0) as f32;
        self.last_tick = now;

        if now - self.last_brightness_check > 2.0 {
            self.last_brightness_check = now;
            let b: Vec<_> = self.overlays.iter().map(|o| o.brightness()).collect();
            if b != self.brightness {
                self.brightness = b;
                self.needs_redraw = true;
            }
        }

        let overlay_hwnds: Vec<HWND> = self.overlays.iter().map(|o| o.hwnd).collect();
        let (windows, overlay_z) = current_windows(&overlay_hwnds, self.my_pid);
        let ids: Vec<isize> = windows.iter().map(|w| key(w.hwnd)).collect();
        let id_set: HashSet<isize> = ids.iter().copied().collect();

        self.bodies.retain(|k, _| id_set.contains(k));
        self.retry_after.retain(|k, _| id_set.contains(k));
        self.captures.retain(|k, _| id_set.contains(k));
        for w in &windows {
            let first = self.first_frame;
            self.bodies.entry(key(w.hwnd)).or_insert_with(|| JelloBody::new(w.rect, !first));
        }
        self.first_frame = false;

        // Grab the jello where the user presses, so dragging wobbles from that point.
        let mut mouse = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut mouse);
        }
        let mouse = [mouse.x as f32, mouse.y as f32];
        let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
        if down && !self.was_mouse_down {
            if let Some(hit) = windows.iter().find(|w| w.rect.contains(mouse)) {
                if let Some(b) = self.bodies.get_mut(&key(hit.hwnd)) {
                    b.grab(mouse);
                }
            }
        } else if !down {
            for b in self.bodies.values_mut() {
                b.grab_index = None;
            }
        }
        self.was_mouse_down = down;

        let mut moved = false;
        for w in &windows {
            if let Some(b) = self.bodies.get_mut(&key(w.hwnd)) {
                moved |= b.step(dt, w.rect, jello_enabled);
            }
        }

        // Start captures front to back so the visible windows get them first.
        let mut started = 0;
        for (rank, w) in windows.iter().enumerate() {
            let k = key(w.hwnd);
            let fps = if rank < FULL_RATE_WINDOWS { FULL_FPS } else { BACKGROUND_FPS };
            if let Some(cap) = self.captures.get_mut(&k) {
                cap.set_fps(fps);
                continue;
            }
            if self.retry_after.get(&k).is_some_and(|&t| t > now) {
                continue;
            }
            if self.captures.len() >= MAX_CAPTURES || started >= MAX_NEW_CAPTURES_PER_TICK {
                continue;
            }
            started += 1;
            match WindowCapture::new(w.hwnd, &self.renderer.rt_device, fps) {
                Ok(cap) => {
                    self.captures.insert(k, cap);
                }
                // Elevated or protected windows can't be captured; don't hammer them.
                Err(_) => {
                    self.retry_after.insert(k, now + 5.0);
                }
            }
        }

        let mut failed = Vec::new();
        for (k, cap) in self.captures.iter_mut() {
            if cap.poll(&self.renderer.device).is_err() {
                failed.push(*k);
            }
        }
        for k in failed {
            self.captures.remove(&k);
            self.retry_after.insert(k, now + 10.0);
        }

        // Skip the GPU work entirely when no mesh moved, no new frame arrived and
        // the window set is unchanged; the overlay keeps showing its last frame.
        let signature = self.captures.values().fold(0u64, |s, c| s.wrapping_add(c.frame_count));
        if !(self.needs_redraw || moved || signature != self.last_frame_signature || ids != self.last_window_ids) {
            return Ok(());
        }
        self.needs_redraw = false;
        self.last_frame_signature = signature;
        self.last_window_ids = ids;

        for (i, overlay) in self.overlays.iter().enumerate() {
            let work = overlay.monitor.work;
            let mut items = Vec::new();
            // Back to front; windows in front of this overlay (other topmost windows) stay real.
            for w in windows.iter().rev().filter(|w| w.z > overlay_z[i] && w.rect.intersects(&work)) {
                let (Some(frame), Some(body)) =
                    (self.captures.get(&key(w.hwnd)).and_then(|c| c.latest.as_ref()), self.bodies.get(&key(w.hwnd)))
                else {
                    continue;
                };
                items.push(renderer::Item { srv: &frame.srv, uv_max: frame.uv_max, pos: &body.pos });
            }
            let (sdr, peak) = self.brightness[i];
            self.renderer.render(
                overlay,
                self.wallpaper.as_ref(),
                &items,
                sdr,
                if hdr_enabled { peak } else { sdr },
                triangles_enabled,
            )?;
        }
        Ok(())
    }
}

/// Window classes that are part of the shell rather than app windows.
const SKIP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "#32768", // menus
    "tooltips_class32",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "NotifyIconOverflowWindow",
];

/// On-screen app windows front to back, plus each overlay's own z position.
fn current_windows(overlays: &[HWND], my_pid: u32) -> (Vec<WindowInfo>, Vec<usize>) {
    unsafe extern "system" fn cb(hwnd: HWND, data: LPARAM) -> BOOL {
        (*(data.0 as *mut Vec<HWND>)).push(hwnd);
        TRUE
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
    }

    let mut overlay_z = vec![0; overlays.len()];
    let mut out = Vec::new();
    for (z, &hwnd) in all.iter().enumerate() {
        if let Some(i) = overlays.iter().position(|&o| o == hwnd) {
            overlay_z[i] = z;
        } else if let Some(rect) = app_window_rect(hwnd, my_pid) {
            out.push(WindowInfo { hwnd, rect, z });
        }
    }
    (out, overlay_z)
}

fn app_window_rect(hwnd: HWND, my_pid: u32) -> Option<Rect> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == my_pid {
            return None;
        }
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        // Click-through windows are other apps' overlays (game bar, GPU stats, ...).
        if ex.contains(WS_EX_TRANSPARENT) {
            return None;
        }
        if ex.contains(WS_EX_LAYERED) {
            let mut alpha = 255u8;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
            if GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
                && flags.contains(LWA_ALPHA)
                && alpha < 3
            {
                return None;
            }
        }
        let mut class = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if SKIP_CLASSES.contains(&class.as_str()) {
            return None;
        }
        // DWM queries last, they cost more than the user32 checks above.
        // Cloaked = on another virtual desktop, or a suspended UWP app.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
        if cloaked != 0 {
            return None;
        }

        // Physical-pixel bounds without the invisible resize border.
        let mut r = RECT::default();
        DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as _, std::mem::size_of::<RECT>() as u32)
            .ok()?;
        let rect = overlay::to_rect(r);
        (rect.w >= 40.0 && rect.h >= 40.0).then_some(rect)
    }
}
