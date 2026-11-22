#![allow(dead_code)] // remove once the achievements UI calls into this module
//! Real GPU shaders for the achievement badges (Direct3D 11 pixel shaders, compiled at run time).
//!
//! The shaders are the reference GLSL of the `achievements` skill (`bubble.js`, `gem.js`) ported line by line to
//! HLSL (see `shaders.rs`): Bronze / Silver / Gold / Platinum = the struck round medal, Diamond / Ruby = the round
//! brilliant, Emulsion = the live soap bubble with the Resource Monitor pulse mark.
//!
//! * One shared hardware D3D11 device (WARP software fallback), created lazily on first use. `D3DCompile` comes from
//!   `d3dcompiler_47.dll`, loaded at run time from System32 (ships with Windows 10+); no files, no network.
//! * `render_batch` draws every item into one atlas render target, copies it once to a staging texture, maps it and
//!   splits it: one GPU to CPU read-back per batch. Textures and buffers are reused between calls; a lost device is
//!   recreated once per call.
//! * Output pixels are premultiplied BGRA, `0xAARRGGBB` as a little-endian `u32`, exactly like `render::Canvas`.
//! * Nothing here panics. If D3D11 is missing or a shader fails to compile, `available()` is false and
//!   `render_batch` returns false (one line goes to `util::diag`); the caller keeps its software badges.
//!
//! Time and reduced motion: the caller passes `time_s` (seconds, free running). With "reduce motion" draw one still
//! frame at the reference's fixed time: `STILL_T_GEM` (9 s) for metals / stones, `STILL_T_BUBBLE` (14 s) for the
//! bubble (see `still_time`). `seed` offsets time per badge (reference: gem `(i % 7) * 5.1`, bubble `(i % 5) * 7.3`).
//! `tilt` is the full view's pointer tilt (about -0.5..0.5 each axis; (0,0) = none) and moves the stone's view vector.
//!
//! Masks (both are optional; a badge without one is simply drawn without that detail):
//! * `icon_mask`: 128x128 alpha bytes (row 0 = top), the icon drawn white on clear in a square that fills the mask.
//!   Metals show it engraved in the middle of the medal face (the square spans 63% of the medal diameter, centred).
//!   Not used on Diamond, Ruby (no etching on stones, per the skill) or Emulsion.
//! * `detail_mask`: accepted for API stability, currently ignored (the round medal has no text band).
//! Textures are cached by pointer + length + a sampled content hash, so pass the same slice for the same icon.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET};

#[path = "shaders.rs"]
mod shaders;

/// Reference still-frame times (reduced motion): gem.js draws t = 9, bubble.js t = 14.
pub const STILL_T_GEM: f32 = 9.0;
pub const STILL_T_BUBBLE: f32 = 14.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Material {
    Bronze,
    Silver,
    Gold,
    Platinum,
    Diamond,
    Ruby,
    Emulsion,
}

/// The fixed time to use for one still frame (reduced motion) for this material.
pub fn still_time(m: Material) -> f32 {
    if m == Material::Emulsion {
        STILL_T_BUBBLE
    } else {
        STILL_T_GEM
    }
}

pub struct Params<'a> {
    pub material: Material,
    /// Side of the square badge in pixels (8..=2048).
    pub size_px: u32,
    pub time_s: f32,
    pub seed: f32,
    pub tilt: (f32, f32),
    /// 128x128 alpha, row 0 = top. Engraved into metal medals.
    pub icon_mask: Option<&'a [u8]>,
    /// NxN alpha, currently ignored (see module docs).
    pub detail_mask: Option<(&'a [u8], u32)>,
}

const MAX_ATLAS: u32 = 4096;
const ICON_N: u32 = 128;

/// Per item constant buffer; mirrors `cbuffer C` in `shaders::COMMON` (5 rows of 4 floats).
#[repr(C)]
#[derive(Clone, Copy)]
struct CbData([f32; 20]);

enum Fail {
    Lost,
    Other(String),
}

struct Atlas {
    rt: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    stage: ID3D11Texture2D,
    w: u32,
    h: u32,
}

struct CachedTex {
    srv: ID3D11ShaderResourceView,
    stamp: u64,
}

struct Gpu {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    hardware: bool,
    vs: ID3D11VertexShader,
    ps_gem: ID3D11PixelShader,
    ps_bubble: ID3D11PixelShader,
    cb: ID3D11Buffer,
    samp: ID3D11SamplerState,
    rast: ID3D11RasterizerState,
    atlas: Option<Atlas>,
    tex: HashMap<u64, CachedTex>,
    frame: u64,
}

enum State {
    Uninit,
    Ready(Box<Gpu>),
    Failed,
}

struct Holder(State);
// The D3D11 objects are only ever touched while holding the mutex.
unsafe impl Send for Holder {}

static GPU: Mutex<Holder> = Mutex::new(Holder(State::Uninit));
static RENDER_FAILS: AtomicU32 = AtomicU32::new(0);
static LOST_COUNT: AtomicU32 = AtomicU32::new(0);

fn lock() -> std::sync::MutexGuard<'static, Holder> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

fn ensure(h: &mut Holder) {
    if let State::Uninit = h.0 {
        h.0 = match Gpu::new() {
            Ok(g) => State::Ready(Box::new(g)),
            Err(e) => {
                crate::util::diag(&format!("achievements gpu: unavailable ({e}); using software badges"));
                State::Failed
            }
        };
    }
}

/// True when D3D11 and both pixel shaders are ready. The first call creates the device and compiles the shaders
/// (a few hundred ms): call it once from a worker thread at start-up to warm up.
pub fn available() -> bool {
    let mut g = lock();
    ensure(&mut g);
    matches!(g.0, State::Ready(_))
}

/// True when running on a real GPU, false on the WARP software rasteriser (slow: keep the frame rate low).
pub fn is_hardware() -> bool {
    let mut g = lock();
    ensure(&mut g);
    matches!(&g.0, State::Ready(gpu) if gpu.hardware)
}

/// Draw every item; `out[i]` receives `size_px * size_px` premultiplied BGRA pixels of `items[i]`.
/// Returns false (and leaves `out` unspecified) if the GPU path is unavailable or failed; never panics.
pub fn render_batch(items: &[Params], out: &mut Vec<Vec<u32>>) -> bool {
    out.clear();
    if items.iter().any(|p| p.size_px < 8 || p.size_px > 2048) {
        return false;
    }
    let mut g = lock();
    ensure(&mut g);
    let mut retried = false;
    loop {
        let res = match &mut g.0 {
            State::Ready(gpu) => gpu.render(items, out),
            _ => return false,
        };
        match res {
            Ok(()) => {
                RENDER_FAILS.store(0, Ordering::Relaxed);
                return true;
            }
            Err(Fail::Lost) if !retried && LOST_COUNT.fetch_add(1, Ordering::Relaxed) < 4 => {
                retried = true;
                crate::util::diag("achievements gpu: device lost, recreating");
                g.0 = State::Uninit;
                ensure(&mut g);
            }
            Err(e) => {
                let msg = match e {
                    Fail::Lost => "device lost".to_string(),
                    Fail::Other(s) => s,
                };
                let n = RENDER_FAILS.fetch_add(1, Ordering::Relaxed) + 1;
                if n <= 3 {
                    crate::util::diag(&format!("achievements gpu: render failed ({msg})"));
                }
                if n >= 3 {
                    g.0 = State::Failed;
                }
                return false;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// run-time shader compiler

type D3DCompileFn = unsafe extern "system" fn(
    *const c_void, // pSrcData
    usize,         // SrcDataSize
    *const u8,     // pSourceName
    *const c_void, // pDefines
    *const c_void, // pInclude
    *const u8,     // pEntrypoint
    *const u8,     // pTarget
    u32,           // Flags1
    u32,           // Flags2
    *mut *mut c_void, // ppCode
    *mut *mut c_void, // ppErrorMsgs
) -> i32;

fn load_d3dcompile() -> Option<D3DCompileFn> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW};
    unsafe {
        let name = crate::util::wide("d3dcompiler_47.dll");
        // LOAD_LIBRARY_SEARCH_SYSTEM32: never pick the DLL up from the app folder or the current directory.
        let h = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0x0000_0800);
        if h.is_null() {
            return None;
        }
        let p = GetProcAddress(h, b"D3DCompile\0".as_ptr())?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, D3DCompileFn>(p))
    }
}

unsafe fn compile(f: D3DCompileFn, src: &str, entry: &'static [u8], target: &'static [u8]) -> Result<Vec<u8>, String> {
    let mut code: *mut c_void = std::ptr::null_mut();
    let mut err: *mut c_void = std::ptr::null_mut();
    let hr = f(
        src.as_ptr() as *const c_void,
        src.len(),
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
        entry.as_ptr(),
        target.as_ptr(),
        0,
        0,
        &mut code,
        &mut err,
    );
    let mut text = String::new();
    if !err.is_null() {
        let b = ID3DBlob::from_raw(err);
        let bytes = std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize());
        let full = String::from_utf8_lossy(bytes);
        // one line: the first line that says "error", else the first line
        let line = full.lines().find(|l| l.contains("error")).or_else(|| full.lines().next()).unwrap_or("");
        text = line.trim().to_string();
    }
    if hr < 0 || code.is_null() {
        if !code.is_null() {
            drop(ID3DBlob::from_raw(code));
        }
        return Err(format!("shader compile failed (hr {:#x}) {}", hr as u32, text));
    }
    let b = ID3DBlob::from_raw(code);
    Ok(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec())
}

// ---------------------------------------------------------------------------------------------------------------

fn metal_rgb(m: Material) -> [f32; 3] {
    // reflectances from gem.js METAL
    match m {
        Material::Bronze => [0.86, 0.52, 0.30],
        Material::Silver => [0.93, 0.94, 0.97],
        Material::Gold => [1.0, 0.77, 0.33],
        Material::Platinum => [0.72, 0.93, 0.91],
        _ => [0.0, 0.0, 0.0],
    }
}

fn kind_of(m: Material) -> f32 {
    match m {
        Material::Diamond => 0.0,
        Material::Ruby => 1.0,
        _ => 2.0, // metals (bubble ignores it)
    }
}

fn hash_mask(b: &[u8], salt: u64) -> u64 {
    // pointer + length + 512 sampled bytes (cheap, catches a reused buffer that now holds another icon)
    let mut h: u64 = 0xcbf29ce484222325 ^ salt;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x100000001b3);
    };
    mix(b.as_ptr() as usize as u64);
    mix(b.len() as u64);
    let step = (b.len() / 512).max(1);
    let mut i = 0;
    while i < b.len() {
        mix(b[i] as u64);
        i += step;
    }
    h
}

/// Box-filtered mip chain of an R8 image (down to 1x1).
fn mip_chain(w: u32, h: u32, data: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut v: Vec<(u32, u32, Vec<u8>)> = vec![(w, h, data.to_vec())];
    loop {
        let (lw, lh) = {
            let l = &v[v.len() - 1];
            (l.0, l.1)
        };
        if lw <= 1 && lh <= 1 {
            break;
        }
        let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
        let mut nd = vec![0u8; (nw * nh) as usize];
        {
            let src = &v[v.len() - 1].2;
            for y in 0..nh {
                for x in 0..nw {
                    let (x0, x1) = ((x * 2).min(lw - 1), (x * 2 + 1).min(lw - 1));
                    let (y0, y1) = ((y * 2).min(lh - 1), (y * 2 + 1).min(lh - 1));
                    let s = src[(y0 * lw + x0) as usize] as u32
                        + src[(y0 * lw + x1) as usize] as u32
                        + src[(y1 * lw + x0) as usize] as u32
                        + src[(y1 * lw + x1) as usize] as u32;
                    nd[(y * nw + x) as usize] = ((s + 2) / 4) as u8;
                }
            }
        }
        v.push((nw, nh, nd));
    }
    v
}

impl Gpu {
    fn new() -> Result<Gpu, String> {
        let compiler = load_d3dcompile().ok_or("d3dcompiler_47.dll not found")?;
        unsafe {
            let levels = [D3D_FEATURE_LEVEL_11_0];
            let mut dev: Option<ID3D11Device> = None;
            let mut ctx: Option<ID3D11DeviceContext> = None;
            let mut hardware = true;
            let r = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            );
            if r.is_err() || dev.is_none() || ctx.is_none() {
                hardware = false;
                dev = None;
                ctx = None;
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_FLAG(0),
                    Some(&levels),
                    D3D11_SDK_VERSION,
                    Some(&mut dev),
                    None,
                    Some(&mut ctx),
                )
                .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
            }
            let dev = dev.ok_or("no device")?;
            let ctx = ctx.ok_or("no context")?;

            let vs_code = compile(compiler, shaders::VS, b"vsmain\0", b"vs_5_0\0").map_err(|e| format!("vs: {e}"))?;
            let gem_code = compile(compiler, &shaders::gem_src(), b"main\0", b"ps_5_0\0").map_err(|e| format!("gem ps: {e}"))?;
            let bub_code = compile(compiler, &shaders::bubble_src(), b"main\0", b"ps_5_0\0").map_err(|e| format!("bubble ps: {e}"))?;

            let mut vs: Option<ID3D11VertexShader> = None;
            dev.CreateVertexShader(&vs_code, None, Some(&mut vs)).map_err(|e| format!("CreateVertexShader: {e}"))?;
            let mut ps_gem: Option<ID3D11PixelShader> = None;
            dev.CreatePixelShader(&gem_code, None, Some(&mut ps_gem)).map_err(|e| format!("CreatePixelShader gem: {e}"))?;
            let mut ps_bubble: Option<ID3D11PixelShader> = None;
            dev.CreatePixelShader(&bub_code, None, Some(&mut ps_bubble)).map_err(|e| format!("CreatePixelShader bubble: {e}"))?;

            let bd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<CbData>() as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut cb: Option<ID3D11Buffer> = None;
            dev.CreateBuffer(&bd, None, Some(&mut cb)).map_err(|e| format!("CreateBuffer: {e}"))?;

            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            let mut samp: Option<ID3D11SamplerState> = None;
            dev.CreateSamplerState(&sd, Some(&mut samp)).map_err(|e| format!("CreateSamplerState: {e}"))?;

            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                FrontCounterClockwise: false.into(),
                DepthBias: 0,
                DepthBiasClamp: 0.0,
                SlopeScaledDepthBias: 0.0,
                DepthClipEnable: true.into(),
                ScissorEnable: false.into(),
                MultisampleEnable: false.into(),
                AntialiasedLineEnable: false.into(),
            };
            let mut rast: Option<ID3D11RasterizerState> = None;
            dev.CreateRasterizerState(&rd, Some(&mut rast)).map_err(|e| format!("CreateRasterizerState: {e}"))?;

            Ok(Gpu {
                dev,
                ctx,
                hardware,
                vs: vs.ok_or("vs")?,
                ps_gem: ps_gem.ok_or("ps")?,
                ps_bubble: ps_bubble.ok_or("ps")?,
                cb: cb.ok_or("cb")?,
                samp: samp.ok_or("sampler")?,
                rast: rast.ok_or("rasterizer")?,
                atlas: None,
                tex: HashMap::new(),
                frame: 0,
            })
        }
    }

    /// Turn a failed D3D call into `Fail`, telling a lost device from any other error.
    fn fail(&self, what: &str, e: windows::core::Error) -> Fail {
        let code = e.code();
        let lost = code == DXGI_ERROR_DEVICE_REMOVED
            || code == DXGI_ERROR_DEVICE_RESET
            || unsafe { self.dev.GetDeviceRemovedReason() }.is_err();
        if lost {
            Fail::Lost
        } else {
            Fail::Other(format!("{what}: {e}"))
        }
    }

    fn make_atlas(&self, w: u32, h: u32) -> Result<Atlas, Fail> {
        unsafe {
            let mut d = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut rt: Option<ID3D11Texture2D> = None;
            self.dev.CreateTexture2D(&d, None, Some(&mut rt)).map_err(|e| self.fail("atlas", e))?;
            let rt = rt.ok_or_else(|| Fail::Other("atlas".into()))?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.dev.CreateRenderTargetView(&rt, None, Some(&mut rtv)).map_err(|e| self.fail("atlas rtv", e))?;
            d.Usage = D3D11_USAGE_STAGING;
            d.BindFlags = 0;
            d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut stage: Option<ID3D11Texture2D> = None;
            self.dev.CreateTexture2D(&d, None, Some(&mut stage)).map_err(|e| self.fail("staging", e))?;
            Ok(Atlas {
                rt,
                rtv: rtv.ok_or_else(|| Fail::Other("rtv".into()))?,
                stage: stage.ok_or_else(|| Fail::Other("staging".into()))?,
                w,
                h,
            })
        }
    }

    /// The R8 mask as a shader resource view (mip chain), cached.
    fn mask_srv(&mut self, data: &[u8], w: u32, h: u32) -> Option<ID3D11ShaderResourceView> {
        if data.len() != (w * h) as usize {
            return None;
        }
        let key = hash_mask(data, (w as u64) << 32 | h as u64);
        if let Some(c) = self.tex.get_mut(&key) {
            c.stamp = self.frame;
            return Some(c.srv.clone());
        }
        if self.tex.len() > 96 {
            let keep = self.frame.saturating_sub(300);
            self.tex.retain(|_, c| c.stamp >= keep);
            if self.tex.len() > 96 {
                self.tex.clear();
            }
        }
        let chain = mip_chain(w, h, data);
        let subs: Vec<D3D11_SUBRESOURCE_DATA> = chain
            .iter()
            .map(|(lw, _, d)| D3D11_SUBRESOURCE_DATA { pSysMem: d.as_ptr() as *const c_void, SysMemPitch: *lw, SysMemSlicePitch: 0 })
            .collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: chain.len() as u32,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        unsafe {
            let mut t: Option<ID3D11Texture2D> = None;
            self.dev.CreateTexture2D(&desc, Some(subs.as_ptr()), Some(&mut t)).ok()?;
            let t = t?;
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            self.dev.CreateShaderResourceView(&t, None, Some(&mut srv)).ok()?;
            let srv = srv?;
            self.tex.insert(key, CachedTex { srv: srv.clone(), stamp: self.frame });
            Some(srv)
        }
    }

    fn render(&mut self, items: &[Params], out: &mut Vec<Vec<u32>>) -> Result<(), Fail> {
        self.frame += 1;
        out.resize_with(items.len(), Vec::new);
        let mut start = 0usize;
        while start < items.len() {
            // shelf-pack as many items as fit one atlas
            let mut places: Vec<(u32, u32)> = Vec::new();
            let (mut cx, mut ry, mut rh, mut aw) = (0u32, 0u32, 0u32, 0u32);
            let mut end = start;
            while end < items.len() {
                let s = items[end].size_px;
                if cx + s > MAX_ATLAS {
                    ry += rh;
                    cx = 0;
                    rh = 0;
                }
                if ry + s > MAX_ATLAS {
                    break;
                }
                places.push((cx, ry));
                cx += s;
                rh = rh.max(s);
                aw = aw.max(cx);
                end += 1;
            }
            if end == start {
                return Err(Fail::Other("item does not fit the atlas".into()));
            }
            let ah = ry + rh;
            self.draw_chunk(&items[start..end], &places, aw, ah, &mut out[start..end])?;
            start = end;
        }
        Ok(())
    }

    fn draw_chunk(&mut self, items: &[Params], places: &[(u32, u32)], aw: u32, ah: u32, out: &mut [Vec<u32>]) -> Result<(), Fail> {
        // (re)create the atlas when it is too small; grow in steps of 256 px so it settles
        let need_new = match &self.atlas {
            Some(a) => a.w < aw || a.h < ah,
            None => true,
        };
        if need_new {
            let (cw, ch) = self.atlas.as_ref().map(|a| (a.w, a.h)).unwrap_or((0, 0));
            let w = ((aw.max(cw) + 255) / 256 * 256).min(MAX_ATLAS);
            let h = ((ah.max(ch) + 255) / 256 * 256).min(MAX_ATLAS);
            self.atlas = None;
            self.atlas = Some(self.make_atlas(w, h)?);
        }
        // masks first (they borrow self mutably)
        let mut icons: Vec<Option<ID3D11ShaderResourceView>> = Vec::with_capacity(items.len());
        for p in items {
            let metal = !matches!(p.material, Material::Diamond | Material::Ruby | Material::Emulsion);
            icons.push(match p.icon_mask {
                Some(m) if metal => self.mask_srv(m, ICON_N, ICON_N),
                _ => None,
            });
        }
        let atlas = match &self.atlas {
            Some(a) => a,
            None => return Err(Fail::Other("no atlas".into())),
        };
        unsafe {
            let ctx = &self.ctx;
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.RSSetState(&self.rast);
            ctx.VSSetShader(&self.vs, None);
            ctx.OMSetRenderTargets(Some(&[Some(atlas.rtv.clone())]), None);
            ctx.PSSetSamplers(0, Some(&[Some(self.samp.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.ClearRenderTargetView(&atlas.rtv, &[0.0, 0.0, 0.0, 0.0]);

            let mut cur_bubble: Option<bool> = None;
            for (i, p) in items.iter().enumerate() {
                let s = p.size_px as f32;
                let (x, y) = places[i];
                let bubble = p.material == Material::Emulsion;
                if cur_bubble != Some(bubble) {
                    ctx.PSSetShader(if bubble { &self.ps_bubble } else { &self.ps_gem }, None);
                    cur_bubble = Some(bubble);
                }
                let m = metal_rgb(p.material);
                let mut c = [0f32; 20];
                c[0] = s;
                c[1] = s;
                c[2] = x as f32;
                c[3] = y as f32;
                c[4] = p.time_s;
                c[5] = p.seed;
                c[6] = kind_of(p.material);
                c[7] = 0.0; // uShape (round)
                c[8] = m[0];
                c[9] = m[1];
                c[10] = m[2];
                c[11] = s;
                c[12] = p.tilt.0;
                c[13] = p.tilt.1;
                c[14] = if icons[i].is_some() { 1.0 } else { 0.0 };
                let mut ms = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut ms)).map_err(|e| self.fail("map cb", e))?;
                std::ptr::copy_nonoverlapping(c.as_ptr(), ms.pData as *mut f32, 20);
                ctx.Unmap(&self.cb, 0);
                ctx.PSSetShaderResources(0, Some(&[icons[i].clone()]));
                ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    TopLeftX: x as f32,
                    TopLeftY: y as f32,
                    Width: s,
                    Height: s,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]));
                ctx.Draw(3, 0);
            }
            ctx.PSSetShaderResources(0, Some(&[None]));

            // one copy to the staging texture, one map
            let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: aw, bottom: ah, back: 1 };
            ctx.CopySubresourceRegion(&atlas.stage, 0, 0, 0, 0, &atlas.rt, 0, Some(&bx));
            let mut ms = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&atlas.stage, 0, D3D11_MAP_READ, 0, Some(&mut ms)).map_err(|e| self.fail("map staging", e))?;
            let pitch = ms.RowPitch as usize;
            let base = ms.pData as *const u8;
            if base.is_null() {
                ctx.Unmap(&atlas.stage, 0);
                return Err(Fail::Other("null map".into()));
            }
            for (i, p) in items.iter().enumerate() {
                let n = p.size_px as usize;
                let (x, y) = (places[i].0 as usize, places[i].1 as usize);
                let dst = &mut out[i];
                dst.clear();
                dst.resize(n * n, 0);
                for row in 0..n {
                    let src = std::slice::from_raw_parts(base.add((y + row) * pitch + x * 4), n * 4);
                    let d = &mut dst[row * n..(row + 1) * n];
                    for (px, b) in d.iter_mut().zip(src.chunks_exact(4)) {
                        *px = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    }
                }
            }
            ctx.Unmap(&atlas.stage, 0);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Windows only: compiles both pixel shaders with the real `D3DCompile` and renders every material (WARP on a CI
// runner without a GPU). Fails on a compile error instead of quietly falling back to the flat medal. Writes a
// contact sheet to `target/medal-shots/medals.png` (top row: still frames, bottom row: t = 2.5 s, tilted) so a
// person can look at the result; CI uploads it.

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Material; 7] = [
        Material::Bronze,
        Material::Silver,
        Material::Gold,
        Material::Platinum,
        Material::Diamond,
        Material::Ruby,
        Material::Emulsion,
    ];
    const PX: u32 = 192;

    /// A test icon: a ring with a bar through it, white on clear.
    fn icon() -> Vec<u8> {
        let n = ICON_N as i32;
        let mut v = vec![0u8; (n * n) as usize];
        for y in 0..n {
            for x in 0..n {
                let (dx, dy) = ((x - n / 2) as f32, (y - n / 2) as f32);
                let r = (dx * dx + dy * dy).sqrt();
                let ring = (r - 44.0).abs() < 9.0;
                let bar = dy.abs() < 8.0 && dx.abs() < 40.0;
                if ring || bar {
                    v[(y * n + x) as usize] = 255;
                }
            }
        }
        v
    }

    fn params<'a>(m: Material, t: f32, tilt: (f32, f32), ic: &'a [u8]) -> Params<'a> {
        Params { material: m, size_px: PX, time_s: t, seed: 0.0, tilt, icon_mask: Some(ic), detail_mask: None }
    }

    fn render(gpu: &mut Gpu, items: &[Params]) -> Vec<Vec<u32>> {
        let mut out = Vec::new();
        match gpu.render(items, &mut out) {
            Ok(()) => out,
            Err(Fail::Lost) => panic!("render: device lost"),
            Err(Fail::Other(e)) => panic!("render: {e}"),
        }
    }

    /// (covered fraction, luminance std-dev of covered pixels, mean premultiplied rgb of covered pixels)
    fn stats(px: &[u32]) -> (f32, f32, [f32; 3]) {
        let (mut n, mut s, mut s2, mut rgb) = (0f32, 0f32, 0f32, [0f32; 3]);
        for &p in px {
            let a = (p >> 24) & 0xff;
            if a < 128 {
                continue;
            }
            let (r, g, b) = (((p >> 16) & 0xff) as f32, ((p >> 8) & 0xff) as f32, (p & 0xff) as f32);
            let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            n += 1.0;
            s += l;
            s2 += l * l;
            rgb[0] += r;
            rgb[1] += g;
            rgb[2] += b;
        }
        let cov = n / px.len() as f32;
        if n == 0.0 {
            return (0.0, 0.0, rgb);
        }
        let mean = s / n;
        (cov, (s2 / n - mean * mean).max(0.0).sqrt(), [rgb[0] / n, rgb[1] / n, rgb[2] / n])
    }

    #[test]
    fn shaders_compile_and_every_material_renders() {
        let mut gpu = match Gpu::new() {
            Ok(g) => g,
            Err(e) => panic!("GPU path unavailable, badges would fall back to flat medals: {e}"),
        };
        println!("device: {}", if gpu.hardware { "hardware" } else { "WARP" });
        let ic = icon();
        let still: Vec<Params> = ALL.iter().map(|&m| params(m, still_time(m), (0.0, 0.0), &ic)).collect();
        let live: Vec<Params> = ALL.iter().map(|&m| params(m, 2.5, (0.35, -0.25), &ic)).collect();
        let a = render(&mut gpu, &still);
        let b = render(&mut gpu, &live);
        assert_eq!(a.len(), ALL.len());

        let mut means = Vec::new();
        for (i, m) in ALL.iter().enumerate() {
            assert_eq!(a[i].len(), (PX * PX) as usize);
            let (cov, sd, mean) = stats(&a[i]);
            println!("{m:?}: covered {:.0}%, luminance sd {sd:.1}, mean rgb {:.0} {:.0} {:.0}", cov * 100.0, mean[0], mean[1], mean[2]);
            // a round badge in a square covers pi/4 = 79% at most; anything far off is a broken shader
            assert!(cov > 0.25 && cov < 0.95, "{m:?}: {:.0}% of the tile covered", cov * 100.0);
            // a flat disc has no shading; the real medal / stone / film varies strongly
            assert!(sd > 6.0, "{m:?}: looks flat (luminance sd {sd:.1})");
            assert!(a[i].iter().all(|&p| {
                let al = p >> 24;
                ((p >> 16) & 0xff) <= al && ((p >> 8) & 0xff) <= al && (p & 0xff) <= al
            }), "{m:?}: not premultiplied (colour above alpha)");
            assert_ne!(a[i], b[i], "{m:?}: time and tilt change nothing");
            means.push(mean);
        }
        // the metals must actually differ in colour: gold is warmer (r - b) than silver
        let warm = |c: [f32; 3]| c[0] - c[2];
        assert!(warm(means[2]) > warm(means[1]) + 10.0, "gold and silver look alike: {:?} vs {:?}", means[2], means[1]);

        // contact sheet over the app's dark background
        let (w, h) = (PX * ALL.len() as u32, PX * 2);
        let mut img = vec![0u8; (w * h * 3) as usize];
        for (row, set) in [&a, &b].iter().enumerate() {
            for (i, tile) in set.iter().enumerate() {
                for y in 0..PX {
                    for x in 0..PX {
                        let p = tile[(y * PX + x) as usize];
                        let al = (p >> 24) as f32 / 255.0;
                        let bg = 30.0 * (1.0 - al);
                        let o = (((row as u32 * PX + y) * w + i as u32 * PX + x) * 3) as usize;
                        img[o] = (((p >> 16) & 0xff) as f32 + bg).min(255.0) as u8;
                        img[o + 1] = (((p >> 8) & 0xff) as f32 + bg).min(255.0) as u8;
                        img[o + 2] = ((p & 0xff) as f32 + bg).min(255.0) as u8;
                    }
                }
            }
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("medal-shots");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("medals.png"), png_rgb(w, h, &img)).unwrap();
    }

    /// Minimal PNG writer (8-bit RGB, stored deflate blocks), test use only.
    fn png_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc(&body).to_be_bytes());
        }
        let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
        for y in 0..h as usize {
            raw.push(0);
            raw.extend_from_slice(&rgb[y * w as usize * 3..(y + 1) * w as usize * 3]);
        }
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, blk) in blocks.iter().enumerate() {
            z.push((i + 1 == blocks.len()) as u8);
            let n = blk.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(blk);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}
