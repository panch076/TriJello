#![windows_subsystem = "windows"]

mod capture;
mod overlay;
mod physics;
mod renderer;
mod window_manager;

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, Result, HSTRING, PCWSTR};
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use window_manager::WindowManager;

/// Toggles shared between the tray menu and the frame loop. Cells rather than a
/// borrowed manager, because COM calls in the frame loop can re-enter the window proc.
pub struct Settings {
    pub hdr: Cell<bool>,
    pub jello: Cell<bool>,
    pub triangles: Cell<bool>,
    /// A toggle changed; redraw even if nothing moved.
    pub dirty: Cell<bool>,
    /// Displays, work areas or the wallpaper changed; rebuild the overlays.
    pub rebuild: Cell<bool>,
}

thread_local! {
    pub static SETTINGS: Settings = Settings {
        hdr: Cell::new(true),
        jello: Cell::new(true),
        triangles: Cell::new(true),
        dirty: Cell::new(false),
        rebuild: Cell::new(false),
    };
}

const WM_TRAY: u32 = WM_APP + 1;
const ID_HDR: usize = 1;
const ID_JELLO: usize = 2;
const ID_TRIANGLES: usize = 3;
const ID_QUIT: usize = 9;
const HOTKEY_QUIT: i32 = 1;
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn main() {
    if let Err(e) = run() {
        unsafe {
            MessageBoxW(None, &HSTRING::from(format!("TriJello failed to start:\n\n{e}")), w!("TriJello"), MB_ICONERROR);
        }
    }
}

fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;

        let _mutex = CreateMutexW(None, true, w!("TriJello.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
    }
    if !GraphicsCaptureSession::IsSupported()? {
        return Err(windows::core::Error::new(E_NOTIMPL, "Windows.Graphics.Capture isn't available on this PC."));
    }

    let mut manager = WindowManager::new()?;
    let hwnd = create_tray_window()?;
    add_tray_icon(hwnd);
    unsafe {
        // Panic button, in case a buggy jello triangle ever hides the tray.
        let _ = RegisterHotKey(hwnd, HOTKEY_QUIT, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'J' as u32);
    }

    let result = (|| loop {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        manager.tick()?;
        manager.wait_for_vblank();
    })();

    unsafe {
        let nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() };
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
    result
}

/// Hidden (never shown) top-level window: owns the tray icon and receives
/// display/settings broadcasts, which message-only windows don't get.
fn create_tray_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("TriJelloTray"),
            ..Default::default()
        };
        RegisterClassW(&class);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("TriJelloTray"),
            w!("TriJello"),
            WS_OVERLAPPED,
            0, 0, 0, 0,
            None, None, instance, None,
        )
    }
}

fn add_tray_icon(hwnd: HWND) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: triangle_icon(),
        ..Default::default()
    };
    for (dst, src) in nid.szTip.iter_mut().zip("TriJello △".encode_utf16()) {
        *dst = src;
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

/// A 32×32 pink jello triangle, drawn at runtime so there's no .ico to ship.
fn triangle_icon() -> HICON {
    const S: i32 = 32;
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: S,
                biHeight: -S, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let hdc = GetDC(None);
        let Ok(color) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            ReleaseDC(None, hdc);
            return LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        };
        ReleaseDC(None, hdc);

        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (S * S) as usize);
        for y in 0..S {
            for x in 0..S {
                let u = (x as f32 + 0.5) / S as f32;
                let v = (y as f32 + 0.5) / S as f32;
                // Same mask as the shader: apex top-center, base along the bottom.
                let d = v * 0.5 - (u - 0.5).abs();
                let a = (d * S as f32 + 0.5).clamp(0.0, 1.0);
                // premultiplied BGRA
                let (r, g, b) = (255.0 * a, 79.0 * a, 216.0 * a);
                px[(y * S + x) as usize] =
                    ((a * 255.0) as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
            }
        }
        let mask = CreateBitmap(S, S, 1, 1, None);
        let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(mask);
        let _ = DeleteObject(color);
        icon
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let item = |id: usize, title: PCWSTR, on: bool| {
            let check = if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | check, id, title);
        };
        SETTINGS.with(|s| {
            item(ID_HDR, w!("HDR Boost"), s.hdr.get());
            item(ID_JELLO, w!("Jello Physics"), s.jello.get());
            item(ID_TRIANGLES, w!("Triangles"), s.triangles.get());
        });
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit TriJello\tCtrl+Alt+J"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when you click elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        SETTINGS.with(|s| {
            let flip = |c: &Cell<bool>| c.set(!c.get());
            match cmd.0 as usize {
                ID_HDR => flip(&s.hdr),
                ID_JELLO => flip(&s.jello),
                ID_TRIANGLES => flip(&s.triangles),
                ID_QUIT => PostQuitMessage(0),
                _ => return,
            }
            s.dirty.set(true);
        });
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (lp.0 & 0xffff) as u32;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            SETTINGS.with(|s| s.rebuild.set(true));
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            let action = SYSTEM_PARAMETERS_INFO_ACTION(wp.0 as u32);
            if action == SPI_SETDESKWALLPAPER || action == SPI_SETWORKAREA {
                SETTINGS.with(|s| s.rebuild.set(true));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_HOTKEY if wp.0 as i32 == HOTKEY_QUIT => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted; the tray icon is gone.
            add_tray_icon(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
