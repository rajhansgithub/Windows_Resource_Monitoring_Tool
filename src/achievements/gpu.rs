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
//! * `icon_mask`: ICON_N x ICON_N (512) alpha bytes (row 0 = top), the icon drawn white on clear in a square that fills the mask.
//!   Metals show it engraved in the middle of the medal face (the square spans 63% of the medal diameter, centred).
//!   Not used on Diamond, Ruby (no etching on stones, per the skill) or Emulsion.
//! * `detail_mask`: accepted for API stability, currently ignored (the round medal has no text band).
//! Textures are cached by pointer + length + a sampled content hash, so pass the same slice for the same icon.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// Age: dust, scratches, cobweb, each 0..1 (`core::Wear`). Zero = a new medal. Ignored by the bubble.
    pub wear: [f32; 3],
    /// ICON_N x ICON_N alpha, row 0 = top. Engraved into metal medals.
    pub icon_mask: Option<&'a [u8]>,
    /// NxN alpha, currently ignored (see module docs).
    pub detail_mask: Option<(&'a [u8], u32)>,
    /// 0 = the badge. Metals and stones only: 1 = its age layer L (premultiplied dust + fibres + cobweb, a = coverage),
    /// 2 = its age haze (r = amount, g = tint x 4, b = grey). `wear` as for the badge; see `shaders::ageLayers`.
    /// Coins: 0 only (their age layers are made and kept here, see `Gpu::coin_ages`).
    pub layer: u8,
    /// Draw the Dial family coin (3D, `shaders::COIN`) instead of the flat medal / brilliant / bubble.
    pub coin: Option<Coin<'a>>,
}

/// The Dial family coin: one frame, one item. Bronze to Platinum hold a CPU die (the icon etched on it), Diamond and
/// Ruby a stone in a raised collet, Emulsion the bubble in an open ring (it never ages).
#[derive(Clone, Copy)]
pub struct Coin<'a> {
    /// Orientation in degrees (preview `mat`): yaw about the screen's vertical axis, then pitch about the horizontal.
    pub yaw: f32,
    pub pitch: f32,
    /// Camera half-size in coin radii: 1.24 as the preview (the coin turned fully stays inside).
    pub ext: f32,
    /// RING_N x RING_N alpha, row 0 = top, spanning the face: the tier text round the front (paint-filled).
    pub ring: Option<&'a [u8]>,
    /// RING_N x RING_N alpha spanning 0.92 of the face: the name, tier and maker on the reverse (engraved).
    pub back: Option<&'a [u8]>,
    /// The size (px) the dust and cobweb are drawn for; 0 = `size_px`. The full view keeps it while it drops pixels to
    /// hold the frame rate, so the age layer is not drawn again.
    pub age_px: u32,
}

/// Side of the coin's ring and back text masks.
pub const RING_N: u32 = 1024;
/// The coin programs: (stone, age layer). Stone 0 die, 1 Diamond, 2 Ruby, 3 Emulsion; the layer programs only need the
/// geometry, which Diamond and Ruby share.
const COIN_VARIANTS: [(u8, bool); 6] = [(0, false), (1, false), (2, false), (3, false), (0, true), (1, true)];
static COINS_OK: AtomicBool = AtomicBool::new(false);

fn coin_stone(m: Material) -> u8 {
    match m {
        Material::Diamond => 1,
        Material::Ruby => 2,
        Material::Emulsion => 3,
        _ => 0,
    }
}

/// Index into `COIN_VARIANTS` for an item (layer items: 1 front, 2 back).
fn coin_variant(m: Material, layer: u8) -> usize {
    let st = coin_stone(m);
    if layer > 0 {
        if st == 0 { 4 } else { 5 }
    } else {
        st as usize
    }
}

/// Coin metals (preview CMET): the four metal tiers, platinum round the Diamond, rose gold round the Ruby, pearl for Emulsion.
fn coin_metal(m: Material) -> [f32; 3] {
    match m {
        Material::Diamond => [0.72, 0.93, 0.91],
        Material::Ruby => [0.98, 0.62, 0.52],
        Material::Emulsion => [0.92, 0.93, 0.95],
        _ => metal_rgb(m),
    }
}

/// Row-major coin-to-world rotation (preview `mat(yaw, pitch, 0)` = Ry * Rx).
fn coin_rot(yaw: f32, pitch: f32) -> [[f32; 3]; 3] {
    let (a, b) = (yaw.to_radians(), pitch.to_radians());
    let (ca, sa, cb, sb) = (a.cos(), a.sin(), b.cos(), b.sin());
    [[ca, sa * sb, sa * cb], [0.0, cb, -sb], [-sa, ca * sb, ca * cb]]
}

/// Does this coin item carry dust or cobweb? (Same threshold as `ageLayers`: dust + web * 1.3 < .002 = clean.)
fn coin_aged(p: &Params) -> bool {
    p.coin.is_some() && p.layer == 0 && p.material != Material::Emulsion && p.wear[0] + p.wear[2] * 1.3 >= 0.002
}

/// Side of a coin's age layer: one texel per screen pixel of the face, in steps of 16 so near sizes share it.
fn age_n(size_px: u32, ext: f32) -> u32 {
    let n = (size_px as f32 / ext.max(0.5)).round() as u32;
    ((n + 15) / 16 * 16).clamp(32, 1024)
}

/// True once every coin program is compiled and ready (see `warm`). Until then, draw the medals.
pub fn coins_ready() -> bool {
    COINS_OK.load(Ordering::Relaxed)
}

const MAX_ATLAS: u32 = 4096;
/// Icon mask side in px. 512 so the engraving stays crisp in the full view (128 showed texel steps on the walls).
pub const ICON_N: u32 = 512;

/// Per item constant buffer; mirrors `cbuffer C` in `shaders::COMMON` (5 rows of 4 floats).
#[repr(C)]
#[derive(Clone, Copy)]
struct CbData([f32; 20]);

enum Fail {
    Lost,
    /// A coin program is still being compiled (`warm`): not an error, the caller draws the medal this time.
    NotReady,
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
    /// `COIN_VARIANTS`, filled from the disk cache at creation and by `warm` (compiled off the lock).
    ps_coin: [Option<ID3D11PixelShader>; 6],
    cb: ID3D11Buffer,
    /// The coin's second constant buffer (`cbuffer K`): rotation, camera, second metal, flags.
    cbk: ID3D11Buffer,
    /// Premultiplied "over" (ONE, INV_SRC_ALPHA): the Emulsion coin is drawn over its bubble. Every other item lands on
    /// cleared pixels, where this is the same as no blending.
    blend: ID3D11BlendState,
    /// Coin age layers (front | back, BGRA), by `coin_ages` key.
    age: HashMap<u64, CachedTex>,
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
            Err(Fail::NotReady) => return false,
            Err(Fail::Lost) if !retried && LOST_COUNT.fetch_add(1, Ordering::Relaxed) < 4 => {
                retried = true;
                crate::util::diag("achievements gpu: device lost, recreating");
                g.0 = State::Uninit;
                ensure(&mut g);
            }
            Err(e) => {
                let msg = match e {
                    Fail::Lost => "device lost".to_string(),
                    Fail::NotReady => "not ready".to_string(),
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

/// Put the age layers (see `shaders::ageLayers`, `gpu::Params::layer`) on a premultiplied BGRA frame: the haze, then
/// dust, fibres and cobweb. Same maths as the shader's `ageApply`.
pub fn age_apply(f: &mut [u32], l: &[u32], h: &[u32]) {
    let n = f.len().min(l.len()).min(h.len());
    let ch = |v: u32, s: u32| ((v >> s) & 255) as f32 / 255.0;
    const GREY: [f32; 3] = [0.15, 0.145, 0.135];
    for i in 0..n {
        let (p, lp, hp) = (f[i], l[i], h[i]);
        let a8 = p >> 24;
        if a8 == 0 || (lp == 0 && hp & 0x00ff_0000 == 0) {
            continue;
        }
        let a = a8 as f32 / 255.0;
        let mut c = [ch(p, 16), ch(p, 8), ch(p, 0)];
        let hm = ch(hp, 16);
        if hm > 0.0 {
            let (g1, g2) = (ch(hp, 8) / 4.0, ch(hp, 0));
            let mx = c[0].max(c[1]).max(c[2]).max(0.001);
            for k in 0..3 {
                let m = (c[k] / mx * g1 + GREY[k] * g2) * a;
                c[k] += (m - c[k]) * hm;
            }
        }
        let la = ch(lp, 24);
        if la > 0.0 || lp != 0 {
            let lc = [ch(lp, 16), ch(lp, 8), ch(lp, 0)];
            for k in 0..3 {
                c[k] = c[k] * (1.0 - la) + lc[k] * a;
            }
        }
        let to = |x: f32| (x.clamp(0.0, a) * 255.0 + 0.5) as u32;
        f[i] = a8 << 24 | to(c[0]) << 16 | to(c[1]) << 8 | to(c[2]);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// run-time shader compiler

/// Compiled shaders are kept in %LOCALAPPDATA%\ResourceMonitor\shader-cache, named by a hash of their source: the
/// big medal shader takes seconds to compile, so only the first start after an update pays for it (and that happens
/// on a background thread, see `warm`). A bad or old file is just compiled again.
const CACHE_VER: u64 = 1;

fn fnv64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn cache_path(src: &str, entry: &[u8], target: &[u8]) -> std::path::PathBuf {
    let key = fnv64(&[src.as_bytes(), entry, target, &CACHE_VER.to_le_bytes()]);
    crate::util::data_dir().join("shader-cache").join(format!("{key:016x}.cso"))
}

/// The compiled shader from the disk cache only (no compiler): None if it is not there yet.
fn cached_only(src: &str, entry: &[u8], target: &[u8]) -> Option<Vec<u8>> {
    let b = std::fs::read(cache_path(src, entry, target)).ok()?;
    (b.len() > 32 && &b[..4] == b"DXBC").then_some(b)
}

unsafe fn cached_compile(f: D3DCompileFn, src: &str, entry: &'static [u8], target: &'static [u8]) -> Result<Vec<u8>, String> {
    if let Some(b) = cached_only(src, entry, target) {
        return Ok(b);
    }
    let path = cache_path(src, entry, target);
    let dir = path.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let code = compile(f, src, entry, target, 0)?;
    let _ = std::fs::create_dir_all(&dir);
    let _ = crate::util::write_atomic(&path, &code);
    Ok(code)
}

/// Make the device and compile the shaders now, on the calling thread. Called once from a background thread a few
/// seconds after start-up, so opening the achievements window never waits for the compiler.
/// Then the coin programs: each is compiled WITHOUT holding the GPU lock (the window keeps drawing medals meanwhile) and
/// put in place in a moment. The first start after an update pays for it here; later starts load them from the cache.
pub fn warm() {
    if !available() || coins_ready() {
        return;
    }
    let Some(f) = load_d3dcompile() else { return };
    for (i, &(stone, layer)) in COIN_VARIANTS.iter().enumerate() {
        let have = matches!(&lock().0, State::Ready(g) if g.ps_coin[i].is_some());
        if have {
            continue;
        }
        let t0 = std::time::Instant::now();
        let code = unsafe { cached_compile(f, &shaders::coin_src(stone, layer), b"main\0", b"ps_5_0\0") };
        match code {
            Ok(code) => {
                let mut g = lock();
                if let State::Ready(gpu) = &mut g.0 {
                    gpu.install_coin(i, &code);
                }
            }
            Err(e) => {
                crate::util::diag(&format!("achievements gpu: coin shader {stone}/{layer}: {e}"));
                return;
            }
        }
        if t0.elapsed().as_millis() > 200 {
            crate::util::diag(&format!("achievements gpu: coin shader {stone}/{layer} compiled in {} ms", t0.elapsed().as_millis()));
        }
    }
}

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

unsafe fn compile(f: D3DCompileFn, src: &str, entry: &'static [u8], target: &'static [u8], flags: u32) -> Result<Vec<u8>, String> {
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
        flags,
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

            let vs_code = cached_compile(compiler, shaders::VS, b"vsmain\0", b"vs_5_0\0").map_err(|e| format!("vs: {e}"))?;
            let gem_code = cached_compile(compiler, &shaders::gem_src(), b"main\0", b"ps_5_0\0").map_err(|e| format!("gem ps: {e}"))?;
            let bub_code = cached_compile(compiler, &shaders::bubble_src(), b"main\0", b"ps_5_0\0").map_err(|e| format!("bubble ps: {e}"))?;

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
            let bk = D3D11_BUFFER_DESC { ByteWidth: 64, ..bd };
            let mut cbk: Option<ID3D11Buffer> = None;
            dev.CreateBuffer(&bk, None, Some(&mut cbk)).map_err(|e| format!("CreateBuffer K: {e}"))?;

            let rt = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let bld = D3D11_BLEND_DESC { AlphaToCoverageEnable: false.into(), IndependentBlendEnable: false.into(), RenderTarget: [rt; 8] };
            let mut blend: Option<ID3D11BlendState> = None;
            dev.CreateBlendState(&bld, Some(&mut blend)).map_err(|e| format!("CreateBlendState: {e}"))?;

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

            let mut g = Gpu {
                dev,
                ctx,
                hardware,
                vs: vs.ok_or("vs")?,
                ps_gem: ps_gem.ok_or("ps")?,
                ps_bubble: ps_bubble.ok_or("ps")?,
                ps_coin: Default::default(),
                cb: cb.ok_or("cb")?,
                cbk: cbk.ok_or("cb K")?,
                blend: blend.ok_or("blend")?,
                age: HashMap::new(),
                samp: samp.ok_or("sampler")?,
                rast: rast.ok_or("rasterizer")?,
                atlas: None,
                tex: HashMap::new(),
                frame: 0,
            };
            // coin programs already compiled by an earlier start (or before a lost device): no compiler here
            COINS_OK.store(false, Ordering::Relaxed);
            for (i, &(stone, layer)) in COIN_VARIANTS.iter().enumerate() {
                if let Some(code) = cached_only(&shaders::coin_src(stone, layer), b"main\0", b"ps_5_0\0") {
                    g.install_coin(i, &code);
                }
            }
            Ok(g)
        }
    }

    /// Put one compiled coin program in place; all six = coins ready.
    fn install_coin(&mut self, i: usize, code: &[u8]) {
        let mut ps: Option<ID3D11PixelShader> = None;
        if unsafe { self.dev.CreatePixelShader(code, None, Some(&mut ps)) }.is_ok() {
            self.ps_coin[i] = ps;
        }
        COINS_OK.store(self.ps_coin.iter().all(|p| p.is_some()), Ordering::Relaxed);
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

    /// A BGRA image (premultiplied `0xAARRGGBB`, row 0 = top) as an immutable shader resource, no mips.
    fn bgra_srv(&self, px: &[u32], w: u32, h: u32) -> Result<ID3D11ShaderResourceView, Fail> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let sub = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const c_void, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        unsafe {
            let mut t: Option<ID3D11Texture2D> = None;
            self.dev.CreateTexture2D(&desc, Some(&sub), Some(&mut t)).map_err(|e| self.fail("age texture", e))?;
            let t = t.ok_or_else(|| Fail::Other("age texture".into()))?;
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            self.dev.CreateShaderResourceView(&t, None, Some(&mut srv)).map_err(|e| self.fail("age srv", e))?;
            srv.ok_or_else(|| Fail::Other("age srv".into()))
        }
    }

    /// The age layer of every aged coin in `items` (None for the rest). Dust and cobweb stay put on the coin, so each
    /// coin's layer (front | back, face on) is drawn once per size, wear step and seed and kept; a frame only samples it.
    fn coin_ages(&mut self, items: &[Params]) -> Result<Vec<Option<ID3D11ShaderResourceView>>, Fail> {
        let key_of = |p: &Params| -> (u64, u32) {
            let ext = p.coin.as_ref().map_or(1.24, |c| c.ext);
            let ring = p.coin.as_ref().and_then(|c| c.ring).map_or(0, |r| hash_mask(r, 7));
            let ap = p.coin.as_ref().map_or(0, |c| c.age_px);
            let n = age_n(if ap > 0 { ap } else { p.size_px }, ext);
            let geo = [coin_variant(p.material, 1) as u8];
            let w: Vec<u8> = p.wear.iter().flat_map(|v| v.to_bits().to_le_bytes()).collect();
            (fnv64(&[&geo, &n.to_le_bytes(), &w, &ring.to_le_bytes()]), n)
        };
        let mut need: Vec<(u64, usize, u32)> = Vec::new();
        for (i, p) in items.iter().enumerate() {
            if !coin_aged(p) {
                continue;
            }
            let (k, n) = key_of(p);
            if let Some(t) = self.age.get_mut(&k) {
                t.stamp = self.frame;
            } else if !need.iter().any(|x| x.0 == k) {
                need.push((k, i, n));
            }
        }
        if !need.is_empty() {
            let mut layers: Vec<Params> = Vec::with_capacity(need.len() * 2);
            for &(_, i, n) in &need {
                let p = &items[i];
                for layer in [1u8, 2] {
                    layers.push(Params {
                        material: p.material,
                        size_px: n,
                        time_s: 0.0,
                        seed: 0.0,
                        tilt: (0.0, 0.0),
                        wear: p.wear,
                        icon_mask: None,
                        detail_mask: None,
                        layer,
                        coin: Some(Coin { yaw: 0.0, pitch: 0.0, ext: 1.0, ring: p.coin.and_then(|c| c.ring), back: None, age_px: 0 }),
                    });
                }
            }
            let mut out = Vec::new();
            self.render(&layers, &mut out)?;
            if self.age.len() + need.len() > 48 {
                let keep = self.frame.saturating_sub(120);
                self.age.retain(|_, c| c.stamp >= keep);
                if self.age.len() + need.len() > 48 {
                    self.age.clear();
                }
            }
            for (j, &(k, _, n)) in need.iter().enumerate() {
                let (f, b, n) = (&out[2 * j], &out[2 * j + 1], n as usize);
                let mut px = vec![0u32; 2 * n * n];
                for y in 0..n {
                    px[y * 2 * n..y * 2 * n + n].copy_from_slice(&f[y * n..(y + 1) * n]);
                    px[y * 2 * n + n..(y + 1) * 2 * n].copy_from_slice(&b[y * n..(y + 1) * n]);
                }
                let srv = self.bgra_srv(&px, 2 * n as u32, n as u32)?;
                self.age.insert(k, CachedTex { srv, stamp: self.frame });
            }
        }
        Ok(items.iter().map(|p| if coin_aged(p) { self.age.get(&key_of(p).0).map(|t| t.srv.clone()) } else { None }).collect())
    }

    fn render(&mut self, items: &[Params], out: &mut Vec<Vec<u32>>) -> Result<(), Fail> {
        if items.iter().any(|p| p.coin.is_some() && self.ps_coin[coin_variant(p.material, p.layer)].is_none()) {
            return Err(Fail::NotReady);
        }
        let ages = self.coin_ages(items)?;
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
            self.draw_chunk(&items[start..end], &ages[start..end], &places, aw, ah, &mut out[start..end])?;
            start = end;
        }
        Ok(())
    }

    fn draw_chunk(&mut self, items: &[Params], ages: &[Option<ID3D11ShaderResourceView>], places: &[(u32, u32)], aw: u32, ah: u32, out: &mut [Vec<u32>]) -> Result<(), Fail> {
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
        let mut texts: Vec<[Option<ID3D11ShaderResourceView>; 2]> = Vec::with_capacity(items.len());
        for p in items {
            let metal = !matches!(p.material, Material::Diamond | Material::Ruby | Material::Emulsion);
            icons.push(match p.icon_mask {
                Some(m) if metal && !(p.coin.is_some() && p.layer > 0) => self.mask_srv(m, ICON_N, ICON_N),
                _ => None,
            });
            let c = p.coin.as_ref();
            let ring = c.and_then(|c| c.ring).and_then(|m| self.mask_srv(m, RING_N, RING_N));
            let back = c.and_then(|c| c.back).filter(|_| p.layer == 0).and_then(|m| self.mask_srv(m, RING_N, RING_N));
            texts.push([ring, back]);
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
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone()), Some(self.cbk.clone())]));
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.ClearRenderTargetView(&atlas.rtv, &[0.0, 0.0, 0.0, 0.0]);

            let put = |buf: &ID3D11Buffer, v: &[f32]| -> Result<(), Fail> {
                let mut ms = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut ms)).map_err(|e| self.fail("map cb", e))?;
                std::ptr::copy_nonoverlapping(v.as_ptr(), ms.pData as *mut f32, v.len());
                ctx.Unmap(buf, 0);
                Ok(())
            };
            let view = |x: f32, y: f32, s: f32| {
                ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: x, TopLeftY: y, Width: s, Height: s, MinDepth: 0.0, MaxDepth: 1.0 }]));
            };
            for (i, p) in items.iter().enumerate() {
                let s = p.size_px as f32;
                let (x, y) = places[i];
                let m = if p.coin.is_some() { coin_metal(p.material) } else { metal_rgb(p.material) };
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
                c[16] = p.wear[0];
                c[17] = p.wear[1];
                c[18] = p.wear[2];
                c[19] = p.layer as f32;
                if let Some(cn) = &p.coin {
                    if p.material == Material::Emulsion && p.layer == 0 {
                        // the bubble first, sized so its outline is the ring sphere's (radius .685 under the same eye)
                        let bs = s * 0.55605;
                        let (bx, by) = (x as f32 + (s - bs) * 0.5, y as f32 + (s - bs) * 0.5);
                        let mut cb = c;
                        cb[0] = bs;
                        cb[1] = bs;
                        cb[2] = bx;
                        cb[3] = by;
                        cb[11] = bs;
                        put(&self.cb, &cb)?;
                        ctx.PSSetShader(&self.ps_bubble, None);
                        view(bx, by, bs);
                        ctx.Draw(3, 0);
                    }
                    let r = coin_rot(cn.yaw, cn.pitch);
                    let on = |b: bool| if b { 1.0 } else { 0.0 };
                    let k = [
                        r[0][0], r[0][1], r[0][2], cn.ext,
                        r[1][0], r[1][1], r[1][2], on(texts[i][0].is_some()),
                        r[2][0], r[2][1], r[2][2], on(texts[i][1].is_some()),
                        m[0], m[1], m[2], on(ages[i].is_some()),
                    ];
                    put(&self.cbk, &k)?;
                    ctx.PSSetShader(self.ps_coin[coin_variant(p.material, p.layer)].as_ref(), None);
                    ctx.PSSetShaderResources(0, Some(&[icons[i].clone(), texts[i][0].clone(), texts[i][1].clone(), ages[i].clone()]));
                } else {
                    ctx.PSSetShader(if p.material == Material::Emulsion { &self.ps_bubble } else { &self.ps_gem }, None);
                    ctx.PSSetShaderResources(0, Some(&[icons[i].clone(), None, None, None]));
                }
                put(&self.cb, &c)?;
                view(x as f32, y as f32, s);
                ctx.Draw(3, 0);
            }
            ctx.PSSetShaderResources(0, Some(&[None, None, None, None]));

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
        Params { material: m, size_px: PX, time_s: t, seed: 0.0, tilt, wear: [0.0; 3], icon_mask: Some(ic), detail_mask: None, layer: 0, coin: None }
    }

    fn render(gpu: &mut Gpu, items: &[Params]) -> Vec<Vec<u32>> {
        let mut out = Vec::new();
        match gpu.render(items, &mut out) {
            Ok(()) => out,
            Err(Fail::Lost) => panic!("render: device lost"),
            Err(Fail::NotReady) => panic!("render: coin shader not ready"),
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

    /// Timings for the "lags and freezes" check (printed, not asserted; CI runs on WARP, so draw times are pessimistic).
    #[test]
    fn perf_numbers() {
        use std::time::Instant;
        let f = load_d3dcompile().expect("d3dcompiler_47.dll");
        let src = shaders::gem_src();
        // optimisation level 1 is the default (flags 0); level 0 = 1 << 14, level 3 = 1 << 15
        for (name, flags) in [("O1", 0u32), ("O0", 1 << 14), ("O3", 1 << 15)] {
            let t = Instant::now();
            let a = unsafe { compile(f, &src, b"main\0", b"ps_5_0\0", flags) }.expect("gem");
            println!("perf: gem ps compile {name} {:.0} ms, {} bytes", t.elapsed().as_secs_f64() * 1e3, a.len());
        }
        let t = Instant::now();
        let c = unsafe { compile(f, &shaders::bubble_src(), b"main\0", b"ps_5_0\0", 0) }.expect("bubble");
        println!("perf: bubble ps compile {:.0} ms, {} bytes", t.elapsed().as_secs_f64() * 1e3, c.len());
        let t = Instant::now();
        let mut gpu = Gpu::new().expect("gpu");
        println!("perf: Gpu::new {:.0} ms ({})", t.elapsed().as_secs_f64() * 1e3, if gpu.hardware { "hardware" } else { "WARP" });
        let t = Instant::now();
        drop(Gpu::new().expect("gpu"));
        println!("perf: Gpu::new again (shader cache) {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        let ic = icon();
        let mut tiles: Vec<Params> = Vec::new();
        for i in 0..42 {
            let m = ALL[i % 6];
            let mut p = params(m, still_time(m), (0.0, 0.0), &ic);
            p.size_px = 76;
            p.wear = [0.6, (i as f32) / 42.0, 0.7];
            tiles.push(p);
        }
        let t = Instant::now();
        render(&mut gpu, &tiles);
        println!("perf: 42 aged tiles at 76 px {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        for (name, wear) in [("clean", [0.0f32, 0.3, 0.0]), ("dust", [0.8, 0.3, 0.0]), ("web", [0.0, 0.3, 0.9]), ("aged", [0.8, 0.3, 0.9])] {
            let mut p = params(Material::Gold, 1.0, (0.1, 0.1), &ic);
            p.size_px = 560;
            p.wear = wear;
            render(&mut gpu, &[p]);
            let t = Instant::now();
            for k in 0..5 {
                let mut q = params(Material::Gold, 1.0 + k as f32 * 0.016, (0.1, 0.1), &ic);
                q.size_px = 560;
                q.wear = wear;
                render(&mut gpu, &[q]);
            }
            println!("perf: full view 560 px {name}: {:.1} ms a frame", t.elapsed().as_secs_f64() * 1e3 / 5.0);
        }
    }

    /// The full view draws the clean medal and puts the age layers on it (`age_apply`): it must look like the medal
    /// drawn aged in one go, and cost about what a clean frame does.
    #[test]
    fn age_layers_match_the_aged_shader() {
        use std::time::Instant;
        let mut gpu = Gpu::new().expect("gpu");
        let ic = icon();
        for m in [Material::Gold, Material::Bronze, Material::Ruby] {
            let p = |layer: u8, wear: [f32; 3]| {
                let mut q = params(m, 1.0, (0.1, 0.1), &ic);
                q.size_px = 560;
                q.wear = wear;
                q.layer = layer;
                q
            };
            let w = [0.8, 0.3, 0.9];
            let one = render(&mut gpu, &[p(0, w)]).swap_remove(0);
            let lay = render(&mut gpu, &[p(1, w), p(2, w)]);
            let t = Instant::now();
            let mut f = render(&mut gpu, &[p(0, [0.0, 0.3, 0.0])]).swap_remove(0);
            age_apply(&mut f, &lay[0], &lay[1]);
            println!("perf: {m:?} full view 560 px aged via layers: {:.1} ms (draw + apply)", t.elapsed().as_secs_f64() * 1e3);
            let (mut worst, mut sum, mut n) = (0i32, 0f64, 0f64);
            for (a, b) in one.iter().zip(f.iter()) {
                for s in [0, 8, 16, 24] {
                    let d = (((a >> s) & 255) as i32 - ((b >> s) & 255) as i32).abs();
                    worst = worst.max(d);
                    sum += d as f64;
                    n += 1.0;
                }
            }
            println!("perf: {m:?} layers vs aged shader: mean diff {:.2}, worst {worst} (of 255)", sum / n);
            assert!(sum / n < 1.0 && worst <= 24, "{m:?}: layers differ from the aged shader (mean {:.2}, worst {worst})", sum / n);
        }
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

    /// A test ring mask: a band along the bottom at the ring text's radius.
    fn ring_mask() -> Vec<u8> {
        let n = RING_N as i32;
        let mut v = vec![0u8; (n * n) as usize];
        for y in 0..n {
            for x in 0..n {
                let (dx, dy) = ((x - n / 2) as f32 / (n / 2) as f32, (y - n / 2) as f32 / (n / 2) as f32);
                let r = (dx * dx + dy * dy).sqrt();
                if (r - 0.775).abs() < 0.022 && dy > 0.3 && (dx * 40.0).sin() > 0.0 {
                    v[(y * n + x) as usize] = 255;
                }
            }
        }
        v
    }

    /// The Dial family coins: every program compiles with the real D3DCompile, every tier renders (clean, turned, aged),
    /// dust shows on metals and stones and never on Emulsion. Contact sheet: target/medal-shots/coins.png.
    #[test]
    fn coins_compile_and_every_tier_renders() {
        let mut gpu = match Gpu::new() {
            Ok(g) => g,
            Err(e) => panic!("GPU path unavailable: {e}"),
        };
        let f = load_d3dcompile().expect("d3dcompiler_47.dll");
        for (i, &(st, l)) in COIN_VARIANTS.iter().enumerate() {
            let t0 = std::time::Instant::now();
            let code = unsafe { compile(f, &shaders::coin_src(st, l), b"main\0", b"ps_5_0\0", 0) }.unwrap_or_else(|e| panic!("coin {st}/{l}: {e}"));
            println!("coin program stone {st} layer {l}: {} ms, {} bytes", t0.elapsed().as_millis(), code.len());
            gpu.install_coin(i, &code);
        }
        assert!(gpu.ps_coin.iter().all(|p| p.is_some()));
        let (ic, ring) = (icon(), ring_mask());
        let coin = |m: Material, t: f32, yaw: f32, pitch: f32, wear: [f32; 3], px: u32| Params {
            material: m,
            size_px: px,
            time_s: t,
            seed: 0.0,
            tilt: (0.0, 0.0),
            wear,
            icon_mask: Some(&ic),
            detail_mask: None,
            layer: 0,
            coin: Some(Coin { yaw, pitch, ext: 1.24, ring: Some(&ring), back: Some(&ring), age_px: 0 }),
        };
        let old = [0.85, 0.4, 1.0];
        let a = render(&mut gpu, &ALL.map(|m| coin(m, 9.0, 28.0, -16.0, [0.0; 3], PX)));
        let b = render(&mut gpu, &ALL.map(|m| coin(m, 2.5, 160.0, 25.0, [0.0; 3], PX)));
        let c = render(&mut gpu, &ALL.map(|m| coin(m, 9.0, 28.0, -16.0, old, PX)));
        for (i, m) in ALL.iter().enumerate() {
            let (cov, sd, mean) = stats(&a[i]);
            let (cov2, sd2, mean2) = stats(&c[i]);
            println!("coin {m:?}: covered {:.0}%, sd {sd:.1}, mean {:.0} {:.0} {:.0}; aged sd {sd2:.1}, mean {:.0} {:.0} {:.0}", cov * 100.0, mean[0], mean[1], mean[2], mean2[0], mean2[1], mean2[2]);
            // the coin fills 1/1.24 of the tile's width, seen at an angle: about 40% of the square
            assert!(cov > 0.25 && cov < 0.7, "{m:?}: {:.0}% of the tile covered", cov * 100.0);
            assert!(sd > 6.0, "{m:?}: looks flat (sd {sd:.1})");
            for set in [&a, &b, &c] {
                assert!(set[i].iter().all(|&p| {
                    let al = p >> 24;
                    ((p >> 16) & 0xff) <= al && ((p >> 8) & 0xff) <= al && (p & 0xff) <= al
                }), "{m:?}: not premultiplied");
            }
            assert_ne!(a[i], b[i], "{m:?}: turning changes nothing");
            assert!((cov - cov2).abs() < 0.01, "{m:?}: age changed the outline");
            if *m == Material::Emulsion {
                assert_eq!(a[i], c[i], "the Emulsion coin must not age");
            } else {
                assert_ne!(a[i], c[i], "{m:?}: dust and cobweb change nothing");
            }
        }
        // live cost of the full view (one coin, 512 px, a fresh frame each time)
        for m in [Material::Gold, Material::Diamond, Material::Emulsion] {
            let _ = render(&mut gpu, &[coin(m, 1.0, 20.0, -10.0, old, 512)]);
            let t0 = std::time::Instant::now();
            for k in 0..8 {
                let _ = render(&mut gpu, &[coin(m, 1.0 + k as f32 * 0.016, 20.0 + k as f32, -10.0, old, 512)]);
            }
            println!("coin {m:?} 512 px live frame: {:.1} ms ({})", t0.elapsed().as_secs_f32() * 1000.0 / 8.0, if gpu.hardware { "hardware" } else { "WARP" });
        }
        let (w, h) = (PX * ALL.len() as u32, PX * 3);
        let mut img = vec![0u8; (w * h * 3) as usize];
        for (row, set) in [&a, &b, &c].iter().enumerate() {
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
        std::fs::write(dir.join("coins.png"), png_rgb(w, h, &img)).unwrap();
        // half size into the log too (base64 between markers): the sheet can be looked at without the artifact
        let (hw, hh) = (w / 2, h / 2);
        let mut half = vec![0u8; (hw * hh * 3) as usize];
        for y in 0..hh {
            for x in 0..hw {
                for ch in 0..3 {
                    let at = |xx: u32, yy: u32| img[((yy * w + xx) * 3 + ch) as usize] as u32;
                    half[((y * hw + x) * 3 + ch) as usize] = ((at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1) + 2) / 4) as u8;
                }
            }
        }
        let png = png_rgb(hw, hh, &half);
        const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::with_capacity(png.len() * 4 / 3 + 4);
        for c in png.chunks(3) {
            let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                b64.push(if k <= c.len() { B64[((v >> (18 - 6 * k)) & 63) as usize] as char } else { '=' });
            }
        }
        println!("COINS_PNG_B64_BEGIN");
        for line in b64.as_bytes().chunks(4000) {
            println!("{}", std::str::from_utf8(line).unwrap());
        }
        println!("COINS_PNG_B64_END");
    }

    /// Age (dust, scratches, cobweb) changes metals and stones: duller, greyer; the bubble never ages.
    #[test]
    fn wear_shows_on_metals_and_stones() {
        let mut gpu = match Gpu::new() {
            Ok(g) => g,
            Err(e) => panic!("GPU path unavailable: {e}"),
        };
        let ic = icon();
        for m in [Material::Gold, Material::Diamond, Material::Emulsion] {
            let clean = params(m, still_time(m), (0.0, 0.0), &ic);
            let mut old = params(m, still_time(m), (0.0, 0.0), &ic);
            old.wear = [0.85, 0.4, 1.0];
            let r = render(&mut gpu, &[clean, old]);
            let (c0, sd0, m0) = stats(&r[0]);
            let (c1, sd1, m1) = stats(&r[1]);
            let sat = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
            println!("{m:?} wear: covered {:.0}% -> {:.0}%, sd {sd0:.1} -> {sd1:.1}, saturation {:.1} -> {:.1}", c0 * 100.0, c1 * 100.0, sat(m0), sat(m1));
            assert!((c0 - c1).abs() < 0.01, "{m:?}: wear changed the outline");
            assert!(r[1].iter().all(|&p| {
                let al = p >> 24;
                ((p >> 16) & 0xff) <= al && ((p >> 8) & 0xff) <= al && (p & 0xff) <= al
            }), "{m:?}: worn medal not premultiplied");
            if m == Material::Emulsion {
                assert_eq!(r[0], r[1], "the bubble must not age");
            } else {
                assert_ne!(r[0], r[1], "{m:?}: wear changes nothing");
                if m == Material::Gold {
                    assert!(sat(m1) < sat(m0), "gold: dust should grey the colour ({:.1} -> {:.1})", sat(m0), sat(m1));
                }
            }
        }
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
