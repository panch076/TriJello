use windows::core::{w, Interface, Result};
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D11::{ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::physics::Rect;
use crate::renderer::Renderer;

/// One physical display.
pub struct Monitor {
    pub handle: HMONITOR,
    pub bounds: Rect,
    /// Excludes the taskbar, which stays real and clickable.
    pub work: Rect,
    device_name: [u16; 32],
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(m, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            out.push(Monitor {
                handle: m,
                bounds: to_rect(info.monitorInfo.rcMonitor),
                work: to_rect(info.monitorInfo.rcWork),
                device_name: info.szDevice,
            });
        }
        TRUE
    }
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

pub fn to_rect(r: RECT) -> Rect {
    Rect { x: r.left as f32, y: r.top as f32, w: (r.right - r.left) as f32, h: (r.bottom - r.top) as f32 }
}

/// Click-through, always-on-top, HDR-capable overlay that replaces the desktop's
/// look on one display's work area.
pub struct OverlayWindow {
    pub hwnd: HWND,
    pub monitor: Monitor,
    pub swapchain: IDXGISwapChain1,
    /// Flip-model buffer 0 always means "the current back buffer", so one view lasts forever.
    pub rtv: ID3D11RenderTargetView,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

impl OverlayWindow {
    pub fn new(monitor: Monitor, renderer: &Renderer, dcomp: &IDCompositionDevice) -> Result<Self> {
        let r = monitor.work;
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(overlay_wndproc),
                hInstance: instance.into(),
                lpszClassName: w!("TriJelloOverlay"),
                ..Default::default()
            };
            RegisterClassW(&class); // fails harmlessly when already registered

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED | WS_EX_NOREDIRECTIONBITMAP,
                w!("TriJelloOverlay"),
                w!("TriJello"),
                WS_POPUP,
                r.x as i32, r.y as i32, r.w as i32, r.h as i32,
                None, None, instance, None,
            )?;
            // Layered + transparent = mouse clicks fall through to the real windows underneath.
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: r.w as u32,
                Height: r.h as u32,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let factory: IDXGIFactory2 = renderer.dxgi_device.GetAdapter()?.GetParent()?;
            let swapchain = factory.CreateSwapChainForComposition(&renderer.device, &desc, None)?;
            // scRGB: linear, 1.0 = 80 nits, values above SDR white are real HDR highlights.
            if let Ok(sc3) = swapchain.cast::<IDXGISwapChain3>() {
                let _ = sc3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709);
            }

            let back: ID3D11Texture2D = swapchain.GetBuffer(0)?;
            let mut rtv = None;
            renderer.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            let rtv = rtv.unwrap();

            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(Self { hwnd, monitor, swapchain, rtv, _target: target, _visual: visual })
        }
    }

    /// (multiplier for SDR white, multiplier for "HDR boost") in scRGB units.
    /// Both are 1.0 on a display that isn't in HDR mode.
    pub fn brightness(&self) -> (f32, f32) {
        let Some((max_nits, hdr)) = output_info(self.monitor.handle) else { return (1.0, 1.0) };
        if !hdr {
            return (1.0, 1.0);
        }
        let sdr = sdr_white_nits(&self.monitor.device_name) / 80.0;
        (sdr, (max_nits / 80.0).clamp(sdr, 50.0))
    }
}

unsafe extern "system" fn overlay_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Never take focus or hit-testing away from the real windows underneath.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// (peak nits, HDR active) for the DXGI output that drives this monitor.
fn output_info(monitor: HMONITOR) -> Option<(f32, bool)> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(o6) = output.cast::<IDXGIOutput6>() {
                    if let Ok(d) = o6.GetDesc1() {
                        if d.Monitor == monitor {
                            return Some((d.MaxLuminance, d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020));
                        }
                    }
                }
                o += 1;
            }
            a += 1;
        }
        None
    }
}

/// The "SDR content brightness" slider in Windows HDR settings, in nits.
fn sdr_white_nits(device_name: &[u16; 32]) -> f32 {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return 80.0;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            .is_err()
        {
            return 80.0;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 || src.viewGdiDeviceName != *device_name {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
            white.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                size: std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                adapterId: p.targetInfo.adapterId,
                id: p.targetInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) == 0 {
                return white.SDRWhiteLevel as f32 / 1000.0 * 80.0;
            }
        }
        80.0
    }
}
