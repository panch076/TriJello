use windows::core::{s, Interface, Result, HSTRING, PCSTR};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

use crate::overlay::OverlayWindow;
use crate::physics::{self, Rect, GRID_N, VERTS};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Vertex {
    pub pos: [f32; 2],
    /// 0..1 across the window, for the triangle mask
    pub uv: [f32; 2],
    /// where to sample the captured buffer (uv scaled to the covered part)
    pub tex: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    viewport: [f32; 2],
    boost: f32,
    triangle: f32,
}

const SHADER_SOURCE: &str = r#"
cbuffer U : register(b0) { float2 viewport; float boost; float tri; };
Texture2D tex : register(t0);
SamplerState smp : register(s0);

struct VIn { float2 pos : POSITION; float2 uv : TEXCOORD0; float2 tex : TEXCOORD1; };
struct VOut { float4 position : SV_Position; float2 uv : TEXCOORD0; float2 tex : TEXCOORD1; };

VOut vmain(VIn v) {
    VOut o;
    float2 p = v.pos / viewport;
    o.position = float4(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = v.uv;
    o.tex = v.tex;
    return o;
}

float3 toLinear(float3 c) {
    return lerp(pow((c + 0.055) / 1.055, 2.4), c / 12.92, step(c, 0.04045));
}

float4 fmain(VOut i) : SV_Target {
    float4 c = tex.Sample(smp, i.tex);  // sRGB-encoded, premultiplied
    float3 rgb = c.a > 0.0 ? toLinear(saturate(c.rgb / c.a)) * c.a : 0.0;
    float mask = 1.0;
    if (tri > 0.5) {
        // Apex at top-center, base along the bottom edge. d > 0 inside.
        float d = i.uv.y * 0.5 - abs(i.uv.x - 0.5);
        float w = max(fwidth(d), 1e-5);
        mask = smoothstep(-w, w, d);
    }
    return float4(rgb * boost, c.a) * mask;
}
"#;

pub struct Item<'a> {
    pub srv: &'a ID3D11ShaderResourceView,
    pub uv_max: [f32; 2],
    /// GRID_N*GRID_N, global pixels
    pub pos: &'a [[f32; 2]; VERTS],
}

pub struct Wallpaper {
    pub srv: ID3D11ShaderResourceView,
    pub width: f32,
    pub height: f32,
}

pub struct Renderer {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub dxgi_device: IDXGIDevice,
    pub rt_device: IDirect3DDevice,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    sampler: ID3D11SamplerState,
    wallpaper_uniforms: ID3D11Buffer,
    window_uniforms: ID3D11Buffer,
    /// All of a frame's vertices, uploaded with one Map: the wallpaper quad, then each window's grid.
    vertices: std::cell::RefCell<(ID3D11Buffer, usize)>,
    staging: std::cell::RefCell<Vec<Vertex>>,
    grid_indices: ID3D11Buffer,
    grid_index_count: u32,
    quad_indices: ID3D11Buffer,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.unwrap();
            let context = context.unwrap();
            let dxgi_device: IDXGIDevice = device.cast()?;
            let rt_device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)?.cast()?;

            let vs_blob = compile(s!("vmain"), s!("vs_5_0"))?;
            let ps_blob = compile(s!("fmain"), s!("ps_5_0"))?;
            let mut vs = None;
            device.CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(blob_bytes(&ps_blob), None, Some(&mut ps))?;

            let elements = [
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: s!("POSITION"),
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    ..Default::default()
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: s!("TEXCOORD"),
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    AlignedByteOffset: 8,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    ..Default::default()
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: s!("TEXCOORD"),
                    SemanticIndex: 1,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    AlignedByteOffset: 16,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    ..Default::default()
                },
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, blob_bytes(&vs_blob), Some(&mut layout))?;

            // Premultiplied alpha "over"
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: TRUE,
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&bd, Some(&mut blend))?;

            // Jello meshes can fold over, so never cull.
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: TRUE,
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&rd, Some(&mut raster))?;

            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sd, Some(&mut sampler))?;

            let cb_size = std::mem::size_of::<Uniforms>() as u32;
            let wallpaper_uniforms = make_buffer(&device, cb_size, D3D11_BIND_CONSTANT_BUFFER, None)?;
            let window_uniforms = make_buffer(&device, cb_size, D3D11_BIND_CONSTANT_BUFFER, None)?;
            let capacity = 4 + 16 * VERTS;
            let vertices = make_vertex_buffer(&device, capacity)?;

            let mut idx: Vec<u16> = Vec::new();
            for y in 0..GRID_N - 1 {
                for x in 0..GRID_N - 1 {
                    let i = (y * GRID_N + x) as u16;
                    let n = GRID_N as u16;
                    idx.extend_from_slice(&[i, i + 1, i + n, i + 1, i + n + 1, i + n]);
                }
            }
            let grid_indices = make_buffer(&device, (idx.len() * 2) as u32, D3D11_BIND_INDEX_BUFFER, Some(idx.as_ptr() as _))?;
            let q: [u16; 6] = [0, 1, 2, 1, 3, 2];
            let quad_indices = make_buffer(&device, 12, D3D11_BIND_INDEX_BUFFER, Some(q.as_ptr() as _))?;

            Ok(Self {
                device,
                context,
                dxgi_device,
                rt_device,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                blend: blend.unwrap(),
                raster: raster.unwrap(),
                sampler: sampler.unwrap(),
                wallpaper_uniforms,
                window_uniforms,
                vertices: std::cell::RefCell::new((vertices, capacity)),
                staging: std::cell::RefCell::new(Vec::with_capacity(capacity)),
                grid_index_count: idx.len() as u32,
                grid_indices,
                quad_indices,
            })
        }
    }

    pub fn render(
        &self,
        overlay: &OverlayWindow,
        wallpaper: Option<&Wallpaper>,
        windows: &[Item],
        sdr: f32,
        boost: f32,
        triangles: bool,
    ) -> Result<()> {
        unsafe {
            let ctx = &self.context;
            let rtv = &overlay.rtv;
            let work = overlay.monitor.work;
            let viewport = [work.w, work.h];
            ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 0.0]);

            // Without the wallpaper we can't hide the real windows' corners, so draw
            // nothing and let the real desktop show through untouched.
            let Some(wp) = wallpaper else {
                return overlay.swapchain.Present(0, DXGI_PRESENT(0)).ok();
            };

            // Build every vertex for this overlay, then upload them in one go.
            let mut staging = self.staging.borrow_mut();
            staging.clear();
            let [u0, v0, u1, v1] = wallpaper_uvs(overlay.monitor.bounds, work, wp.width, wp.height);
            staging.extend_from_slice(&[
                Vertex { pos: [0.0, 0.0], uv: [0.0, 0.0], tex: [u0, v0] },
                Vertex { pos: [work.w, 0.0], uv: [1.0, 0.0], tex: [u1, v0] },
                Vertex { pos: [0.0, work.h], uv: [0.0, 1.0], tex: [u0, v1] },
                Vertex { pos: [work.w, work.h], uv: [1.0, 1.0], tex: [u1, v1] },
            ]);
            let uvs = physics::uvs();
            for item in windows {
                staging.extend(item.pos.iter().zip(uvs).map(|(p, &uv)| Vertex {
                    pos: [p[0] - work.x, p[1] - work.y],
                    uv,
                    tex: [uv[0] * item.uv_max[0], uv[1] * item.uv_max[1]],
                }));
            }
            let mut vb = self.vertices.borrow_mut();
            if staging.len() > vb.1 {
                *vb = (make_vertex_buffer(&self.device, staging.len())?, staging.len());
            }
            write_buffer(ctx, &vb.0, &staging)?;
            write_buffer(ctx, &self.wallpaper_uniforms, &[Uniforms { viewport, boost: sdr, triangle: 0.0 }])?;
            write_buffer(
                ctx,
                &self.window_uniforms,
                &[Uniforms { viewport, boost, triangle: if triangles { 1.0 } else { 0.0 } }],
            )?;

            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: work.w,
                Height: work.h,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            let stride = std::mem::size_of::<Vertex>() as u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(vb.0.clone())), Some(&stride), Some(&offset));

            let wall_cb = Some(self.wallpaper_uniforms.clone());
            ctx.VSSetConstantBuffers(0, Some(&[wall_cb.clone()]));
            ctx.PSSetConstantBuffers(0, Some(&[wall_cb]));
            ctx.IASetIndexBuffer(&self.quad_indices, DXGI_FORMAT_R16_UINT, 0);
            ctx.PSSetShaderResources(0, Some(&[Some(wp.srv.clone())]));
            ctx.DrawIndexed(6, 0, 0);

            let win_cb = Some(self.window_uniforms.clone());
            ctx.VSSetConstantBuffers(0, Some(&[win_cb.clone()]));
            ctx.PSSetConstantBuffers(0, Some(&[win_cb]));
            ctx.IASetIndexBuffer(&self.grid_indices, DXGI_FORMAT_R16_UINT, 0);
            for (i, item) in windows.iter().enumerate() {
                ctx.PSSetShaderResources(0, Some(&[Some(item.srv.clone())]));
                ctx.DrawIndexed(self.grid_index_count, 0, (4 + i * VERTS) as i32);
            }

            // Unbind so captured buffers aren't held by the pipeline after we release them.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            overlay.swapchain.Present(0, DXGI_PRESENT(0)).ok()
        }
    }

    /// The current desktop wallpaper as a texture. Desktop icons aren't included.
    pub fn load_wallpaper(&self) -> Option<Wallpaper> {
        let path = wallpaper_path()?;
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let decoder = wic
                .CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
                .ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let bgra = WICConvertBitmapSource(&GUID_WICPixelFormat32bppPBGRA, &frame).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            bgra.GetSize(&mut w, &mut h).ok()?;
            let (w, h) = (w.min(16384), h.min(16384));
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            let rect = WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
            bgra.CopyPixels(&rect, w * 4, &mut pixels).ok()?;

            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr() as _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
            let mut tex = None;
            self.device.CreateTexture2D(&desc, Some(&data), Some(&mut tex)).ok()?;
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex?, None, Some(&mut srv)).ok()?;
            Some(Wallpaper { srv: srv?, width: w as f32, height: h as f32 })
        }
    }
}

fn wallpaper_path() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let mut buf = [0u16; 260];
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, Some(buf.as_mut_ptr() as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let path = String::from_utf16_lossy(&buf[..n]);
    if !path.is_empty() && std::path::Path::new(&path).exists() {
        return Some(path);
    }
    // Slideshows and Spotlight leave the current image here.
    let fallback = std::env::var("APPDATA").ok()? + r"\Microsoft\Windows\Themes\TranscodedWallpaper";
    std::path::Path::new(&fallback).exists().then_some(fallback)
}

/// UVs that crop the wallpaper "fill"-style to the monitor, then to its work area.
fn wallpaper_uvs(monitor: Rect, work: Rect, iw: f32, ih: f32) -> [f32; 4] {
    let s = (monitor.w / iw).max(monitor.h / ih);
    let (dw, dh) = (iw * s, ih * s);
    let (cx, cy) = (monitor.x + monitor.w / 2.0, monitor.y + monitor.h / 2.0);
    [
        (work.x - cx) / dw + 0.5,
        (work.y - cy) / dh + 0.5,
        (work.x + work.w - cx) / dw + 0.5,
        (work.y + work.h - cy) / dh + 0.5,
    ]
}

unsafe fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut blob = None;
    let mut errors = None;
    let r = D3DCompile(
        SHADER_SOURCE.as_ptr() as _,
        SHADER_SOURCE.len(),
        None,
        None,
        None,
        entry,
        target,
        D3DCOMPILE_OPTIMIZATION_LEVEL3,
        0,
        &mut blob,
        Some(&mut errors),
    );
    if let (Err(e), Some(errors)) = (&r, &errors) {
        let msg = String::from_utf8_lossy(blob_bytes(errors));
        return Err(windows::core::Error::new(e.code(), msg.as_ref()));
    }
    r?;
    Ok(blob.unwrap())
}

unsafe fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
}

unsafe fn make_vertex_buffer(device: &ID3D11Device, count: usize) -> Result<ID3D11Buffer> {
    make_buffer(device, (count * std::mem::size_of::<Vertex>()) as u32, D3D11_BIND_VERTEX_BUFFER, None)
}

unsafe fn write_buffer<T: Copy>(ctx: &ID3D11DeviceContext, buffer: &ID3D11Buffer, data: &[T]) -> Result<()> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    ctx.Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
    std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.pData as *mut T, data.len());
    ctx.Unmap(buffer, 0);
    Ok(())
}

unsafe fn make_buffer(
    device: &ID3D11Device,
    size: u32,
    bind: D3D11_BIND_FLAG,
    init: Option<*const std::ffi::c_void>,
) -> Result<ID3D11Buffer> {
    let dynamic = init.is_none();
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size,
        Usage: if dynamic { D3D11_USAGE_DYNAMIC } else { D3D11_USAGE_IMMUTABLE },
        BindFlags: bind.0 as u32,
        CPUAccessFlags: if dynamic { D3D11_CPU_ACCESS_WRITE.0 as u32 } else { 0 },
        ..Default::default()
    };
    let data = init.map(|p| D3D11_SUBRESOURCE_DATA { pSysMem: p, SysMemPitch: 0, SysMemSlicePitch: 0 });
    let mut buffer = None;
    device.CreateBuffer(&desc, data.as_ref().map(|d| d as *const _), Some(&mut buffer))?;
    Ok(buffer.unwrap())
}
