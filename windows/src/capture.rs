use windows::core::{Interface, Result};
use windows::Foundation::TimeSpan;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
// One buffer we sample from, one Windows renders the next frame into, one spare
// so a fast window never stalls waiting for us to let go.
const BUFFERS: i32 = 3;

/// Latest captured frame of a window. Sampled straight from the capture pool's
/// buffer (no copy); the buffer goes back to Windows when the next frame replaces it.
pub struct FrameTexture {
    pub srv: ID3D11ShaderResourceView,
    /// Portion of the buffer actually covered by content.
    pub uv_max: [f32; 2],
    _frame: Direct3D11CaptureFrame,
}

impl Drop for FrameTexture {
    fn drop(&mut self) {
        let _ = self._frame.Close();
    }
}

/// Streams one window through Windows.Graphics.Capture.
pub struct WindowCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    pool_size: SizeInt32,
    rt_device: IDirect3DDevice,
    /// SRVs for the pool's buffers, reused instead of recreated every frame.
    views: Vec<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    pub fps: u32,
    pub latest: Option<FrameTexture>,
    /// Bumped on every delivered frame, so the renderer can skip redraws when nothing changed.
    pub frame_count: u64,
}

impl WindowCapture {
    pub fn new(hwnd: HWND, rt_device: &IDirect3DDevice, fps: u32) -> Result<Self> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;
        // Free-threaded so we can simply poll TryGetNextFrame from the render loop.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(rt_device, FORMAT, BUFFERS, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        // Windows 11: no yellow capture border around every window.
        let _ = session.SetIsBorderRequired(false);
        let mut cap = Self {
            pool,
            session,
            _item: item,
            pool_size: size,
            rt_device: rt_device.clone(),
            views: Vec::new(),
            fps: 0,
            latest: None,
            frame_count: 0,
        };
        cap.set_fps(fps);
        cap.session.StartCapture()?;
        Ok(cap)
    }

    /// Caps how often Windows delivers frames (Windows 11 24H2+, ignored elsewhere).
    pub fn set_fps(&mut self, fps: u32) {
        if fps != self.fps {
            self.fps = fps;
            let _ = self.session.SetMinUpdateInterval(TimeSpan { Duration: 10_000_000 / fps as i64 });
        }
    }

    /// Drains the frame pool, keeping only the newest frame.
    pub fn poll(&mut self, device: &ID3D11Device) -> Result<()> {
        let mut newest = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            if let Some(old) = newest.replace(f) {
                let _ = old.Close();
            }
        }
        let Some(frame) = newest else { return Ok(()) };

        let content = frame.ContentSize()?;
        let tex: ID3D11Texture2D = unsafe {
            frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()?
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        // Mid-resize the content may not fill the buffer (or may overflow it); sample only the covered part.
        let uv_max = [
            (content.Width.max(1) as f32 / desc.Width as f32).min(1.0),
            (content.Height.max(1) as f32 / desc.Height as f32).min(1.0),
        ];

        let srv = match self.views.iter().find(|(t, _)| *t == tex) {
            Some((_, srv)) => srv.clone(),
            None => {
                if self.views.len() >= 2 * BUFFERS as usize {
                    self.views.clear();
                }
                let mut srv = None;
                unsafe { device.CreateShaderResourceView(&tex, None, Some(&mut srv))? };
                let srv = srv.unwrap();
                self.views.push((tex, srv.clone()));
                srv
            }
        };
        self.latest = Some(FrameTexture { srv, uv_max, _frame: frame });
        self.frame_count += 1;

        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            self.pool_size = content;
            self.views.clear();
            self.pool.Recreate(&self.rt_device, FORMAT, BUFFERS, content)?;
        }
        Ok(())
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        self.latest = None;
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
