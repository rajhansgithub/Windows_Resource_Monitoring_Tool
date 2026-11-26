//! GPU temperature / fan speed straight from the WDDM driver (D3DKMT), the same source
//! Task Manager uses. No vendor DLLs, no admin.
//!
//! `D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERPERFDATA)` fills a `D3DKMT_ADAPTER_PERFDATA`
//! (d3dkmthk.h, WDDM 2.4+). `Temperature` is in deci-degrees Celsius. `Power` is in tenths of a
//! percent of TDP, not watts, so it is deliberately not reported.

use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo, D3DKMT_ADAPTERINFO,
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2, D3DKMT_QUERYADAPTERINFO,
    KMTQAITYPE_ADAPTERPERFDATA,
};

/// Upper bound on adapters we are willing to track.
const MAX_ADAPTERS: u32 = 16;

#[derive(Default)]
pub struct GpuTemp {
    handles: Vec<u32>,
    enumerated: bool,
}

impl Drop for GpuTemp {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl GpuTemp {
    fn close_all(&mut self) {
        for h in self.handles.drain(..) {
            let c = D3DKMT_CLOSEADAPTER { hAdapter: h };
            // SAFETY: handle came from D3DKMTEnumAdapters2 and is closed exactly once.
            unsafe { D3DKMTCloseAdapter(&c) };
        }
    }

    fn enumerate(&mut self) {
        self.close_all();
        self.enumerated = true;
        let mut req = D3DKMT_ENUMADAPTERS2 {
            NumAdapters: 0,
            pAdapters: std::ptr::null_mut(),
        };
        // SAFETY: a null array asks only for the adapter count.
        if unsafe { D3DKMTEnumAdapters2(&mut req) } < 0 || req.NumAdapters == 0 {
            return;
        }
        let n = req.NumAdapters.min(MAX_ADAPTERS);
        // SAFETY: D3DKMT_ADAPTERINFO is plain old data; zeroed is a valid value.
        let mut infos: Vec<D3DKMT_ADAPTERINFO> = vec![unsafe { std::mem::zeroed() }; n as usize];
        req = D3DKMT_ENUMADAPTERS2 {
            NumAdapters: n,
            pAdapters: infos.as_mut_ptr(),
        };
        // SAFETY: pAdapters points to `n` writable entries.
        if unsafe { D3DKMTEnumAdapters2(&mut req) } < 0 {
            return;
        }
        let got = (req.NumAdapters as usize).min(infos.len());
        self.handles.extend(infos[..got].iter().map(|i| i.hAdapter));
    }

    fn query(h: u32) -> Option<D3DKMT_ADAPTER_PERFDATA> {
        // SAFETY: POD struct, zero is a valid value.
        let mut perf: D3DKMT_ADAPTER_PERFDATA = unsafe { std::mem::zeroed() };
        let mut q = D3DKMT_QUERYADAPTERINFO {
            hAdapter: h,
            Type: KMTQAITYPE_ADAPTERPERFDATA,
            pPrivateDriverData: (&mut perf as *mut D3DKMT_ADAPTER_PERFDATA).cast(),
            PrivateDriverDataSize: std::mem::size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
        };
        // SAFETY: pPrivateDriverData points to PrivateDriverDataSize writable bytes.
        (unsafe { D3DKMTQueryAdapterInfo(&mut q) } >= 0).then_some(perf)
    }

    /// Hottest adapter temperature (deg C) and highest fan speed (RPM). None when unsupported.
    pub fn sample(&mut self) -> (Option<f32>, Option<u32>) {
        if !self.enumerated {
            self.enumerate();
        }
        let mut temp: Option<f32> = None;
        let mut fan: Option<u32> = None;
        let mut failed = false;
        for &h in &self.handles {
            match Self::query(h) {
                Some(p) => {
                    let (t, f) = (p.Temperature, p.FanRPM);
                    if t > 0 {
                        let c = t as f32 / 10.0;
                        temp = Some(temp.map_or(c, |m| m.max(c)));
                        if f > 0 {
                            fan = Some(fan.map_or(f, |m| m.max(f)));
                        }
                    }
                }
                None => failed = true,
            }
        }
        if failed {
            // Adapter set changed (hot-plug, driver reset): re-enumerate on the next sample.
            self.close_all();
            self.enumerated = false;
        }
        (temp, fan)
    }
}
