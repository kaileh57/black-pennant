//! Unit `gd-platform` (hard: the only unsafe platform FFI in this crate; the Windows half cannot be run on
//! the owner's machine and is proven only by Windows CI). Small OS queries Godot does not expose.
//! DESIGN.md L1363 (while hosting: `SetProcessInformation` with `ProcessPowerThrottling`, ControlMask
//! `EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION`, StateMask 0, reset afterwards; on macOS an `NSProcessInfo`
//! activity with `UserInitiated | LatencyCritical`), L2016 (Windows DXGI `DedicatedVideoMemory`), L2024 (Mac
//! `thermalState` polled at 1 Hz), L1401 (benchmark memory figures: peak working set and DXGI `CurrentUsage`
//! on Windows; `phys_footprint`, swap growth and peak memory-pressure level on macOS), L1744 (the benchmark
//! logs DPI awareness on Windows).
//!
//! Every function is safe to call on every platform and from any thread, and returns the neutral value where
//! the platform has no answer. All `unsafe` stays inside this file. Dependencies: `windows-sys` (features
//! `Win32_Foundation`, `Win32_System_Threading`, `Win32_UI_HiDpi`, `Win32_System_ProcessStatus`) on Windows;
//! `objc2`, `objc2-foundation` and `libc` on macOS. `windows-sys` 0.61 has no COM interfaces, so the DXGI
//! calls go through vtable slots declared below, checked against the generated `windows` 0.62 bindings of
//! `dxgi.h`, `dxgi1_2.h` and `dxgi1_4.h`.

/// Dedicated video memory of the active adapter in bytes (Windows, DXGI). `None` elsewhere or on failure.
pub fn dedicated_video_memory_bytes() -> Option<u64> {
    #[cfg(windows)]
    { win::with_adapter(|_, desc| desc.DedicatedVideoMemory as u64) }
    #[cfg(not(windows))]
    { None }
}

/// Video memory in use by this process in bytes (Windows, `QueryVideoMemoryInfo` `CurrentUsage`).
pub fn video_memory_in_use_bytes() -> Option<u64> {
    #[cfg(windows)]
    // SAFETY: `with_adapter` hands over a live IDXGIAdapter1 for the duration of the closure.
    { win::with_adapter(|adapter, _| unsafe { win::current_usage(adapter) }).flatten() }
    #[cfg(not(windows))]
    { None }
}

/// Thermal state: 0 nominal, 1 fair, 2 serious, 3 critical (macOS `NSProcessInfo.thermalState`). 0 elsewhere.
pub fn thermal_state() -> u8 {
    #[cfg(target_os = "macos")]
    { mac::thermal_state() }
    #[cfg(not(target_os = "macos"))]
    { 0 }
}

/// Process memory for the benchmark report.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemoryStats {
    /// Windows: peak working set. macOS: `phys_footprint`. Bytes.
    pub footprint_bytes: u64,
    /// macOS: swap in use by the system, bytes; 0 on Windows.
    pub swap_bytes: u64,
    /// macOS memory-pressure level: 1 normal, 2 warning, 4 critical; 0 on Windows.
    pub pressure_level: u8,
}
pub fn memory_stats() -> MemoryStats {
    #[cfg(target_os = "macos")]
    { mac::memory_stats() }
    #[cfg(windows)]
    { win::memory_stats() }
    #[cfg(not(any(windows, target_os = "macos")))]
    { MemoryStats::default() }
}

/// DPI awareness of this process as text for the benchmark log: `"per_monitor_v2"`, `"per_monitor"`,
/// `"system"`, `"unaware"` on Windows; `"n/a"` elsewhere.
pub fn dpi_awareness() -> &'static str {
    #[cfg(windows)]
    { win::dpi_awareness() }
    #[cfg(not(windows))]
    { "n/a" }
}

#[cfg(windows)]
static HOSTING_GUARDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Held while this process hosts a session: asks the OS not to throttle the process or coarsen its timers.
/// Dropping it resets both.
pub struct HostingActivity { pub(crate) token: usize }
impl HostingActivity {
    /// Begin the activity. Never fails: on a platform or OS version without the call the guard is empty.
    pub fn begin() -> HostingActivity {
        #[cfg(target_os = "macos")]
        { HostingActivity { token: mac::begin_activity() } }
        #[cfg(windows)]
        {
            // Throttling is process-wide: the first guard opts out, the last one to drop opts back in.
            if HOSTING_GUARDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { win::set_throttling(true); }
            HostingActivity { token: 1 }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        { HostingActivity { token: 0 } }
    }
}
impl Drop for HostingActivity {
    /// Unit `gd-platform`: reset throttling state or end the `NSProcessInfo` activity when `token` is set.
    fn drop(&mut self) {
        if self.token == 0 { return; }
        #[cfg(target_os = "macos")]
        mac::end_activity(self.token);
        #[cfg(windows)]
        if HOSTING_GUARDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 { win::set_throttling(false); }
        self.token = 0;
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::MemoryStats;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    pub(super) fn thermal_state() -> u8 {
        // `NSProcessInfo` is thread-safe; `processInfo` is the shared instance.
        NSProcessInfo::processInfo().thermalState().0.clamp(0, 3) as u8
    }

    pub(super) fn begin_activity() -> usize {
        let reason = NSString::from_str("Hosting a Black Pennant session");
        let options = NSActivityOptions::UserInitiated | NSActivityOptions::LatencyCritical;
        let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason);
        // The activity lives until `end_activity`: the guard keeps the retained object as a raw pointer.
        Retained::into_raw(token) as usize
    }

    pub(super) fn end_activity(token: usize) {
        let ptr = token as *mut ProtocolObject<dyn NSObjectProtocol>;
        // SAFETY: `token` came from `Retained::into_raw` in `begin_activity` and is consumed exactly once (the
        // guard zeroes it after this call), so ownership of that reference returns here.
        let Some(activity) = (unsafe { Retained::from_raw(ptr) }) else { return };
        // SAFETY: `activity` is the object `beginActivityWithOptions:reason:` returned, as `endActivity:` requires.
        unsafe { NSProcessInfo::processInfo().endActivity(&activity) };
    }

    fn sysctl<T: Copy>(name: &core::ffi::CStr) -> Option<T> {
        let mut value = core::mem::MaybeUninit::<T>::zeroed();
        let mut len = core::mem::size_of::<T>();
        // SAFETY: `value` is writable for `len` bytes and `name` is NUL-terminated; sysctlbyname writes at most
        // `len` bytes and reports the size it wrote.
        let rc = unsafe { libc::sysctlbyname(name.as_ptr(), value.as_mut_ptr().cast(), &mut len, core::ptr::null_mut(), 0) };
        // SAFETY: on success with the full size written, every byte of `T` (plain integers) is initialised; it
        // was zeroed before in any case.
        (rc == 0 && len == core::mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
    }

    pub(super) fn memory_stats() -> MemoryStats {
        let mut info = core::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: the buffer is a `rusage_info_v2`, which is what flavor `RUSAGE_INFO_V2` fills; libc's pointer
        // type (`*mut rusage_info_t`) is the C API's opaque spelling of a pointer to that struct.
        let rc = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
        // SAFETY: zero-initialised plain integers, filled on success.
        let footprint = if rc == 0 { unsafe { info.assume_init() }.ri_phys_footprint } else { 0 };
        let swap = sysctl::<libc::xsw_usage>(c"vm.swapusage").map_or(0, |s| s.xsu_used);
        let pressure = sysctl::<libc::c_int>(c"kern.memorystatus_vm_pressure_level").map_or(0, |p| p.clamp(0, 255) as u8);
        MemoryStats { footprint_bytes: footprint, swap_bytes: swap, pressure_level: pressure }
    }
}

#[cfg(windows)]
mod win {
    use super::MemoryStats;
    use core::ffi::c_void;
    use windows_sys::core::{GUID, HRESULT};
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    };
    use windows_sys::Win32::UI::HiDpi::{AreDpiAwarenessContextsEqual, GetAwarenessFromDpiAwarenessContext, GetDpiAwarenessContextForProcess, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

    // ---- DXGI, by hand ----

    /// `DXGI_ADAPTER_DESC1` (dxgi.h); `AdapterLuid` split into its two fields.
    #[repr(C)]
    #[allow(non_snake_case)]
    #[derive(Clone, Copy)]
    pub(super) struct AdapterDesc1 {
        pub Description: [u16; 128],
        pub VendorId: u32,
        pub DeviceId: u32,
        pub SubSysId: u32,
        pub Revision: u32,
        pub DedicatedVideoMemory: usize,
        pub DedicatedSystemMemory: usize,
        pub SharedSystemMemory: usize,
        pub AdapterLuidLow: u32,
        pub AdapterLuidHigh: i32,
        pub Flags: u32,
    }
    /// `DXGI_QUERY_VIDEO_MEMORY_INFO` (dxgi1_4.h).
    #[repr(C)]
    #[allow(non_snake_case)]
    #[derive(Clone, Copy, Default)]
    struct VideoMemoryInfo { Budget: u64, CurrentUsage: u64, AvailableForReservation: u64, CurrentReservation: u64 }

    const IID_IDXGI_FACTORY1: GUID = GUID::from_u128(0x770aae78_f26f_4dba_a829_253c83d1b387);
    const IID_IDXGI_ADAPTER3: GUID = GUID::from_u128(0x645967a4_1392_4310_a798_8053ce3e93fd);
    const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;
    const DXGI_MEMORY_SEGMENT_GROUP_LOCAL: i32 = 0;

    // Vtable slots. IUnknown: 0 QueryInterface, 1 AddRef, 2 Release. IDXGIObject: 3..=6. IDXGIFactory: 7..=11.
    // IDXGIFactory1: 12 EnumAdapters1, 13 IsCurrent. IDXGIAdapter: 7 EnumOutputs, 8 GetDesc,
    // 9 CheckInterfaceSupport. IDXGIAdapter1: 10 GetDesc1. IDXGIAdapter2: 11 GetDesc2. IDXGIAdapter3:
    // 12, 13 (content protection), 14 QueryVideoMemoryInfo.
    const SLOT_QUERY_INTERFACE: usize = 0;
    const SLOT_RELEASE: usize = 2;
    const SLOT_ENUM_ADAPTERS1: usize = 12;
    const SLOT_GET_DESC1: usize = 10;
    const SLOT_QUERY_VIDEO_MEMORY_INFO: usize = 14;

    type QueryInterfaceFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
    type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
    type EnumAdapters1Fn = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT;
    type GetDesc1Fn = unsafe extern "system" fn(*mut c_void, *mut AdapterDesc1) -> HRESULT;
    type QueryVideoMemoryInfoFn = unsafe extern "system" fn(*mut c_void, u32, i32, *mut VideoMemoryInfo) -> HRESULT;

    #[link(name = "dxgi", kind = "raw-dylib")]
    unsafe extern "system" {
        fn CreateDXGIFactory1(riid: *const GUID, factory: *mut *mut c_void) -> HRESULT;
    }

    /// The function in vtable slot `index` of COM object `this`.
    ///
    /// # Safety
    /// `this` must be a live COM interface pointer whose vtable has at least `index + 1` entries, and `F` must
    /// be that entry's real function-pointer type.
    unsafe fn slot<F: Copy>(this: *mut c_void, index: usize) -> F {
        // SAFETY: per the contract above; a COM object starts with a pointer to its vtable.
        unsafe {
            let vtable = *(this as *const *const usize);
            core::mem::transmute_copy::<usize, F>(&*vtable.add(index))
        }
    }

    /// A COM pointer released on drop.
    struct Com(*mut c_void);
    impl Drop for Com {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a live interface pointer this guard owns one reference to; slot 2 is Release.
                unsafe { slot::<ReleaseFn>(self.0, SLOT_RELEASE)(self.0) };
            }
        }
    }

    /// Run `f` with the adapter Godot renders on and its description. DECISION: Godot's D3D12 and Vulkan
    /// drivers prefer the discrete GPU, so this picks the hardware adapter with the most dedicated memory
    /// (the discrete card on a hybrid laptop, the only adapter elsewhere); software adapters are skipped.
    pub(super) fn with_adapter<R>(f: impl FnOnce(*mut c_void, &AdapterDesc1) -> R) -> Option<R> {
        let mut factory: *mut c_void = core::ptr::null_mut();
        // SAFETY: valid IID and out pointer; on success `factory` holds an IDXGIFactory1 with one reference.
        if unsafe { CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory) } < 0 || factory.is_null() { return None; }
        let factory = Com(factory);
        let mut best: Option<(Com, AdapterDesc1)> = None;
        // At most 16 adapters: a real machine has 1 to 3; the bound keeps a misbehaving driver finite.
        for index in 0..16u32 {
            let mut adapter: *mut c_void = core::ptr::null_mut();
            // SAFETY: `factory` is a live IDXGIFactory1 and slot 12 is EnumAdapters1.
            let hr = unsafe { slot::<EnumAdapters1Fn>(factory.0, SLOT_ENUM_ADAPTERS1)(factory.0, index, &mut adapter) };
            if hr < 0 || adapter.is_null() { break; }
            let adapter = Com(adapter);
            // SAFETY: a zeroed plain-data description that GetDesc1 (slot 10 of this IDXGIAdapter1) fills.
            let mut desc: AdapterDesc1 = unsafe { core::mem::zeroed() };
            if unsafe { slot::<GetDesc1Fn>(adapter.0, SLOT_GET_DESC1)(adapter.0, &mut desc) } < 0 { continue; }
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE != 0 { continue; }
            if best.as_ref().is_none_or(|(_, b)| desc.DedicatedVideoMemory > b.DedicatedVideoMemory) { best = Some((adapter, desc)); }
        }
        let (adapter, desc) = best?;
        Some(f(adapter.0, &desc))
    }

    /// `IDXGIAdapter3::QueryVideoMemoryInfo(0, LOCAL).CurrentUsage`; `None` where Adapter3 is missing.
    ///
    /// # Safety
    /// `adapter` must be a live IDXGIAdapter1 pointer.
    pub(super) unsafe fn current_usage(adapter: *mut c_void) -> Option<u64> {
        let mut a3: *mut c_void = core::ptr::null_mut();
        // SAFETY: QueryInterface (slot 0) on a live interface with a valid IID; success adds one reference.
        if unsafe { slot::<QueryInterfaceFn>(adapter, SLOT_QUERY_INTERFACE)(adapter, &IID_IDXGI_ADAPTER3, &mut a3) } < 0 || a3.is_null() { return None; }
        let a3 = Com(a3);
        let mut info = VideoMemoryInfo::default();
        // SAFETY: `a3` is an IDXGIAdapter3 and slot 14 is QueryVideoMemoryInfo; node 0, local segment group.
        let hr = unsafe { slot::<QueryVideoMemoryInfoFn>(a3.0, SLOT_QUERY_VIDEO_MEMORY_INFO)(a3.0, 0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) };
        (hr >= 0).then_some(info.CurrentUsage)
    }

    // ---- Process ----

    /// Opt out of (`hosting` true) or back into (false) execution-speed and timer-resolution throttling
    /// (L1363). Returns whether the call succeeded.
    pub(super) fn set_throttling(hosting: bool) -> bool {
        let mask = PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION;
        // ControlMask set with StateMask 0 turns throttling off for those bits; ControlMask 0 hands the
        // decision back to the system.
        let state = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ControlMask: if hosting { mask } else { 0 }, StateMask: 0 };
        // SAFETY: this process's pseudo-handle and a correctly sized state struct for this information class.
        unsafe { SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling, (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(), core::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32) != 0 }
    }

    pub(super) fn memory_stats() -> MemoryStats {
        // SAFETY: a zeroed plain-data struct with `cb` set; GetProcessMemoryInfo fills at most `cb` bytes.
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { core::mem::zeroed() };
        counters.cb = core::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } != 0;
        MemoryStats { footprint_bytes: if ok { counters.PeakWorkingSetSize as u64 } else { 0 }, swap_bytes: 0, pressure_level: 0 }
    }

    pub(super) fn dpi_awareness() -> &'static str {
        // SAFETY: plain queries on this process's pseudo-handle and the context value they return.
        unsafe {
            let ctx = GetDpiAwarenessContextForProcess(GetCurrentProcess());
            if AreDpiAwarenessContextsEqual(ctx, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 { return "per_monitor_v2"; }
            match GetAwarenessFromDpiAwarenessContext(ctx) { 2 => "per_monitor", 1 => "system", _ => "unaware" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_safe_and_neutral() {
        assert!(thermal_state() <= 3);
        let m = memory_stats();
        #[cfg(target_os = "macos")]
        { assert!(m.footprint_bytes > 0); assert!(matches!(m.pressure_level, 0 | 1 | 2 | 4)); }
        #[cfg(not(any(windows, target_os = "macos")))]
        assert_eq!(m, MemoryStats::default());
        #[cfg(not(windows))]
        { assert_eq!(dpi_awareness(), "n/a"); assert_eq!(dedicated_video_memory_bytes(), None); assert_eq!(video_memory_in_use_bytes(), None); }
        #[cfg(windows)]
        { let _ = m; assert!(["per_monitor_v2", "per_monitor", "system", "unaware"].contains(&dpi_awareness())); let _ = dedicated_video_memory_bytes(); let _ = video_memory_in_use_bytes(); }
    }

    #[test]
    fn hosting_activity_begins_and_ends() {
        let a = HostingActivity::begin();
        #[cfg(target_os = "macos")]
        assert_ne!(a.token, 0);
        drop(a);
        // Two at once work: each guard owns its own activity.
        let b = HostingActivity::begin();
        let c = HostingActivity::begin();
        drop(c);
        drop(b);
    }
}
