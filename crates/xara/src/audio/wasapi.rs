//! Shared-mode WASAPI. xara asks for 48 kHz 16-bit and lets Windows convert.
//! Mono is tried first; stereo is downmixed on capture and duplicated on playback.

use std::collections::VecDeque;
use std::io;
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Instant;

use super::ratematch::{Queue, TxBuffer};
use super::{Audio, DeviceList, IO_TIMEOUT, Path, RATE, capture_only, virtual_playback_name};

use windows_sys::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows_sys::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows_sys::Win32::System::Variant::VT_LPWSTR;
use windows_sys::core::GUID;

const RPC_E_CHANGED_MODE: i32 = 0x80010106u32 as i32;
const AUDCLNT_S_BUFFER_EMPTY: i32 = 0x08890001u32 as i32;
const SHAREMODE_SHARED: u32 = 0;
const STREAMFLAGS: u32 = 0x80000000 | 0x08000000; // AUTOCONVERTPCM | SRC_DEFAULT_QUALITY
const BUFFER_HNS: i64 = 1_000_000; // 100 ms
const E_RENDER: u32 = 0;
const E_CAPTURE: u32 = 1;
const E_CONSOLE: u32 = 0;
const DEVICE_STATE_ACTIVE: u32 = 1;
const SILENT: u32 = 2;
const CLSCTX: u32 = CLSCTX_ALL;

const CLSID_ENUM: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
const IID_ENUM: GUID = GUID::from_u128(0xA95664D2_9614_4F35_A746_DE8DB63617E6);
const IID_CLIENT: GUID = GUID::from_u128(0x1CB9AD4C_DBFA_4C32_B178_C2F568A703B2);
const IID_RENDER: GUID = GUID::from_u128(0xF294ACFC_3146_4483_A7BF_ADDCA7C260E2);
const IID_CAPTURE: GUID = GUID::from_u128(0xC8ADBD64_E71E_48A0_A4DE_185C395CD317);
const FRIENDLY: GUID = GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0);

#[repr(C)]
struct PropertyKey {
    fmtid: GUID,
    pid: u32,
}

#[repr(C)]
struct WaveFormatEx {
    tag: u16,
    channels: u16,
    rate: u32,
    avg: u32,
    align: u16,
    bits: u16,
    extra: u16,
}

#[repr(C)]
struct UnknownVtbl {
    _q: usize,
    _a: usize,
    release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
}

#[repr(C)]
struct EnumVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    enum_endpoints: unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32, *mut *mut core::ffi::c_void) -> i32,
    get_default: unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32, *mut *mut core::ffi::c_void) -> i32,
    get_device: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *mut *mut core::ffi::c_void) -> i32,
}

#[repr(C)]
struct CollectionVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    get_count: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32) -> i32,
    item: unsafe extern "system" fn(*mut core::ffi::c_void, u32, *mut *mut core::ffi::c_void) -> i32,
}

#[repr(C)]
struct DeviceVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    activate: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *const GUID,
        u32,
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> i32,
    open_store: unsafe extern "system" fn(*mut core::ffi::c_void, u32, *mut *mut core::ffi::c_void) -> i32,
    get_id: unsafe extern "system" fn(*mut core::ffi::c_void, *mut *mut u16) -> i32,
}

#[repr(C)]
struct StoreVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    _count: usize,
    _at: usize,
    get_value: unsafe extern "system" fn(*mut core::ffi::c_void, *const PropertyKey, *mut PROPVARIANT) -> i32,
}

#[repr(C)]
struct ClientVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    initialize:
        unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32, i64, i64, *const WaveFormatEx, *const GUID) -> i32,
    get_buffer_size: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32) -> i32,
    get_stream_latency: unsafe extern "system" fn(*mut core::ffi::c_void, *mut i64) -> i32,
    get_current_padding: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32) -> i32,
    _fmt: usize,
    _mix: usize,
    _period: usize,
    start: unsafe extern "system" fn(*mut core::ffi::c_void) -> i32,
    stop: unsafe extern "system" fn(*mut core::ffi::c_void) -> i32,
    _reset: usize,
    _event: usize,
    get_service: unsafe extern "system" fn(*mut core::ffi::c_void, *const GUID, *mut *mut core::ffi::c_void) -> i32,
}

#[repr(C)]
struct RenderVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    get_buffer: unsafe extern "system" fn(*mut core::ffi::c_void, u32, *mut *mut u8) -> i32,
    release_buffer: unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32) -> i32,
}

#[repr(C)]
struct CaptureVtbl {
    _q: usize,
    _a: usize,
    _r: usize,
    get_buffer:
        unsafe extern "system" fn(*mut core::ffi::c_void, *mut *mut u8, *mut u32, *mut u32, *mut u64, *mut u64) -> i32,
    release_buffer: unsafe extern "system" fn(*mut core::ffi::c_void, u32) -> i32,
}

fn hr(what: &str, code: i32) -> io::Result<()> {
    if code >= 0 { Ok(()) } else { Err(io::Error::other(format!("wasapi {what}: 0x{:08x}", code as u32))) }
}

fn com_init() -> io::Result<()> {
    static ONCE: Once = Once::new();
    static ERR: AtomicI32 = AtomicI32::new(0);
    ONCE.call_once(|| {
        // SAFETY: a null reserved pointer is the documented call. A changed apartment is still usable.
        let code = unsafe { CoInitializeEx(ptr::null(), 0) };
        if code < 0 && code != RPC_E_CHANGED_MODE {
            ERR.store(code, Ordering::Relaxed);
        }
    });
    hr("CoInitializeEx", ERR.load(Ordering::Relaxed))
}

fn release(p: *mut core::ffi::c_void) {
    if p.is_null() {
        return;
    }
    // SAFETY: p is a COM object whose first vtable slots are IUnknown.
    unsafe {
        let v = *(p as *mut *const UnknownVtbl);
        ((*v).release)(p);
    }
}

fn enumerator() -> io::Result<*mut core::ffi::c_void> {
    com_init()?;
    let mut p = ptr::null_mut();
    // SAFETY: the CLSID and IID are the documented MMDevice enumerator, and p receives the object.
    hr("MMDeviceEnumerator", unsafe { CoCreateInstance(&CLSID_ENUM, ptr::null_mut(), CLSCTX, &IID_ENUM, &mut p) })?;
    Ok(p)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    // SAFETY: COM strings are NUL-terminated and live until we free them.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: n counts the characters before the NUL.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

#[derive(Clone)]
struct Dev {
    id: String,
    name: String,
}

fn friendly(dev: *mut core::ffi::c_void) -> Option<String> {
    let v = unsafe { *(dev as *mut *const DeviceVtbl) };
    let mut store = ptr::null_mut();
    // SAFETY: dev is an IMMDevice and store receives an IPropertyStore.
    if unsafe { ((*v).open_store)(dev, 0, &mut store) } < 0 || store.is_null() {
        return None;
    }
    let key = PropertyKey { fmtid: FRIENDLY, pid: 14 };
    let mut var: PROPVARIANT = unsafe { std::mem::zeroed() };
    let sv = unsafe { *(store as *mut *const StoreVtbl) };
    // SAFETY: key and var match GetValue's arguments.
    let code = unsafe { ((*sv).get_value)(store, &key, &mut var) };
    let name = if code >= 0 && unsafe { var.Anonymous.Anonymous.vt } == VT_LPWSTR {
        Some(from_wide(unsafe { var.Anonymous.Anonymous.Anonymous.pwszVal }))
    } else {
        None
    };
    // SAFETY: var was zeroed, then optionally filled by GetValue.
    unsafe { PropVariantClear(&mut var) };
    release(store);
    name.filter(|s| !s.is_empty())
}

fn describe(dev: *mut core::ffi::c_void) -> io::Result<Dev> {
    let v = unsafe { *(dev as *mut *const DeviceVtbl) };
    let mut idp = ptr::null_mut();
    // SAFETY: dev is an IMMDevice and idp receives a task-allocated wide string.
    hr("GetId", unsafe { ((*v).get_id)(dev, &mut idp) })?;
    let id = from_wide(idp);
    // SAFETY: idp came from GetId.
    unsafe { CoTaskMemFree(idp.cast()) };
    let name = friendly(dev).unwrap_or_else(|| id.clone());
    Ok(Dev { id, name })
}

fn endpoints(flow: u32) -> io::Result<Vec<Dev>> {
    let en = enumerator()?;
    let ev = unsafe { *(en as *mut *const EnumVtbl) };
    let mut col = ptr::null_mut();
    // SAFETY: en is an IMMDeviceEnumerator and col receives the collection.
    let code = unsafe { ((*ev).enum_endpoints)(en, flow, DEVICE_STATE_ACTIVE, &mut col) };
    if let Err(e) = hr("EnumAudioEndpoints", code) {
        release(en);
        return Err(e);
    }
    let cv = unsafe { *(col as *mut *const CollectionVtbl) };
    let mut count = 0u32;
    // SAFETY: col is an IMMDeviceCollection.
    let count_hr = unsafe { ((*cv).get_count)(col, &mut count) };
    let mut out = Vec::new();
    if count_hr >= 0 {
        for i in 0..count {
            let mut dev = ptr::null_mut();
            // SAFETY: i is in range and dev receives one IMMDevice.
            if unsafe { ((*cv).item)(col, i, &mut dev) } < 0 || dev.is_null() {
                continue;
            }
            if let Ok(d) = describe(dev) {
                out.push(d);
            }
            release(dev);
        }
    }
    release(col);
    release(en);
    hr("GetCount", count_hr)?;
    Ok(out)
}

fn default_dev(flow: u32) -> io::Result<Dev> {
    let en = enumerator()?;
    let ev = unsafe { *(en as *mut *const EnumVtbl) };
    let mut dev = ptr::null_mut();
    // SAFETY: en is an IMMDeviceEnumerator and dev receives the default endpoint.
    let code = unsafe { ((*ev).get_default)(en, flow, E_CONSOLE, &mut dev) };
    if let Err(e) = hr("GetDefaultAudioEndpoint", code) {
        release(en);
        return Err(e);
    }
    let described = describe(dev);
    release(dev);
    release(en);
    described
}

fn device_by_id(id: &str) -> io::Result<*mut core::ffi::c_void> {
    let en = enumerator()?;
    let ev = unsafe { *(en as *mut *const EnumVtbl) };
    let w = wide(id);
    let mut dev = ptr::null_mut();
    // SAFETY: w is NUL-terminated and dev receives the endpoint.
    let code = unsafe { ((*ev).get_device)(en, w.as_ptr(), &mut dev) };
    release(en);
    hr("GetDevice", code)?;
    Ok(dev)
}

fn pick(devs: &[Dev], want: &str, capture: bool) -> io::Result<Dev> {
    if want.is_empty() || want.eq_ignore_ascii_case("default") {
        if !capture {
            return Err(io::Error::other(
                "name the playback device. the computer's speakers are not used as a transmitter",
            ));
        }
        return default_dev(E_CAPTURE);
    }
    let hits: Vec<_> =
        devs.iter().filter(|d| d.id.eq_ignore_ascii_case(want) || d.name.eq_ignore_ascii_case(want)).cloned().collect();
    match hits.as_slice() {
        [one] => Ok(one.clone()),
        [] => {
            let shown: Vec<_> = devs.iter().take(8).map(|d| d.name.as_str()).collect();
            let kind = if capture { "capture" } else { "playback" };
            Err(io::Error::other(format!("{kind} device {want:?} was not found. devices: {}", shown.join(", "))))
        }
        _ => Err(io::Error::other(format!(
            "{want:?} matches more than one device. use the device id from --list-devices"
        ))),
    }
}

fn pcm(channels: u16) -> WaveFormatEx {
    let align = channels * 2;
    WaveFormatEx { tag: 1, channels, rate: RATE, avg: RATE * u32::from(align), align, bits: 16, extra: 0 }
}

struct Endpoint {
    client: *mut core::ffi::c_void,
    service: *mut core::ffi::c_void,
    channels: u16,
    capacity: u32,
    latency: usize,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if !self.client.is_null() {
            let v = unsafe { *(self.client as *mut *const ClientVtbl) };
            // SAFETY: client is an IAudioClient that was started.
            unsafe { ((*v).stop)(self.client) };
        }
        release(self.service);
        release(self.client);
    }
}

impl Endpoint {
    fn open(id: &str, render: bool) -> io::Result<Self> {
        match Self::try_open(id, render, 1) {
            Ok(ep) => Ok(ep),
            Err(mono) => {
                Self::try_open(id, render, 2).map_err(|stereo| io::Error::other(format!("{stereo} (mono: {mono})")))
            }
        }
    }

    fn try_open(id: &str, render: bool, channels: u16) -> io::Result<Self> {
        let dev = device_by_id(id)?;
        let dv = unsafe { *(dev as *mut *const DeviceVtbl) };
        let mut client = ptr::null_mut();
        // SAFETY: dev is an IMMDevice and client receives an IAudioClient.
        let code = unsafe { ((*dv).activate)(dev, &IID_CLIENT, CLSCTX, ptr::null_mut(), &mut client) };
        release(dev);
        if let Err(e) = hr("Activate", code) {
            return Err(e);
        }
        let opened = Self::start(client, render, channels);
        if opened.is_err() {
            release(client);
        }
        opened
    }

    fn start(client: *mut core::ffi::c_void, render: bool, channels: u16) -> io::Result<Self> {
        let v = unsafe { *(client as *mut *const ClientVtbl) };
        // SAFETY: client is a fresh IAudioClient and the format is a 16-bit PCM WAVEFORMATEX.
        let init = unsafe {
            ((*v).initialize)(client, SHAREMODE_SHARED, STREAMFLAGS, BUFFER_HNS, 0, &pcm(channels), ptr::null())
        };
        hr("Initialize", init)?;
        let mut capacity = 0u32;
        // SAFETY: client is initialized.
        hr("GetBufferSize", unsafe { ((*v).get_buffer_size)(client, &mut capacity) })?;
        let mut hns = 0i64;
        hr("GetStreamLatency", unsafe { ((*v).get_stream_latency)(client, &mut hns) })?;
        let iid = if render { &IID_RENDER } else { &IID_CAPTURE };
        let mut service = ptr::null_mut();
        hr("GetService", unsafe { ((*v).get_service)(client, iid, &mut service) })?;
        if let Err(e) = hr("Start", unsafe { ((*v).start)(client) }) {
            release(service);
            return Err(e);
        }
        let latency = if hns <= 0 { 0 } else { (hns as u64 * u64::from(RATE) / 10_000_000) as usize };
        Ok(Self { client, service, channels, capacity, latency })
    }

    fn padding(&self) -> io::Result<usize> {
        let v = unsafe { *(self.client as *mut *const ClientVtbl) };
        let mut n = 0u32;
        // SAFETY: client is a started IAudioClient.
        hr("GetCurrentPadding", unsafe { ((*v).get_current_padding)(self.client, &mut n) })?;
        Ok(n as usize)
    }

    fn write_frames(&self, samples: &[i16]) -> io::Result<()> {
        let v = unsafe { *(self.service as *mut *const RenderVtbl) };
        let mut data = ptr::null_mut();
        // SAFETY: service is an IAudioRenderClient and samples.len() fits the free space.
        hr("GetBuffer", unsafe { ((*v).get_buffer)(self.service, samples.len() as u32, &mut data) })?;
        let ch = self.channels as usize;
        // SAFETY: GetBuffer just handed us samples.len() frames of ch channels.
        let dst = unsafe { std::slice::from_raw_parts_mut(data as *mut i16, samples.len() * ch) };
        if ch == 1 {
            dst.copy_from_slice(samples);
        } else {
            for (i, s) in samples.iter().enumerate() {
                dst[i * 2] = *s;
                dst[i * 2 + 1] = *s;
            }
        }
        hr("ReleaseBuffer", unsafe { ((*v).release_buffer)(self.service, samples.len() as u32, 0) })
    }

    fn read_into(&self, rx: &mut VecDeque<i16>) -> io::Result<()> {
        let v = unsafe { *(self.service as *mut *const CaptureVtbl) };
        loop {
            let mut data = ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            // SAFETY: service is an IAudioCaptureClient. The position pointers may be null.
            let code = unsafe {
                ((*v).get_buffer)(self.service, &mut data, &mut frames, &mut flags, ptr::null_mut(), ptr::null_mut())
            };
            if code == AUDCLNT_S_BUFFER_EMPTY || frames == 0 {
                return Ok(());
            }
            hr("GetBuffer", code)?;
            let n = frames as usize;
            if flags & SILENT != 0 || data.is_null() {
                rx.extend(std::iter::repeat_n(0, n));
            } else {
                let ch = self.channels as usize;
                // SAFETY: GetBuffer reported n frames of ch channels.
                let src = unsafe { std::slice::from_raw_parts(data as *const i16, n * ch) };
                if ch == 1 {
                    rx.extend(src.iter().copied());
                } else {
                    for pair in src.chunks_exact(2) {
                        rx.push_back(((i32::from(pair[0]) + i32::from(pair[1])) / 2) as i16);
                    }
                }
            }
            hr("ReleaseBuffer", unsafe { ((*v).release_buffer)(self.service, frames) })?;
        }
    }
}

pub struct WasapiIo {
    capture: Option<Endpoint>,
    play: Option<Endpoint>,
    rx: VecDeque<i16>,
}

impl WasapiIo {
    pub fn open(capture: &str, playback: &str, radio: bool, _tx: TxBuffer, _speaker: bool) -> io::Result<Self> {
        let caps = endpoints(E_CAPTURE)?;
        let plays = endpoints(E_RENDER)?;
        let cap = pick(&caps, capture, true)?;
        let play = pick(&plays, playback, false)?;
        if !radio && !virtual_playback_name(&play.name) {
            return Err(not_a_cable(&play.name));
        }
        let capture_ep = Endpoint::open(&cap.id, false)?;
        let play_ep = Endpoint::open(&play.id, true)?;
        say!("wasapi capture {} ({})", cap.name, if capture_ep.channels == 1 { "mono" } else { "stereo" });
        say!("wasapi playback {} ({})", play.name, if play_ep.channels == 1 { "mono" } else { "stereo" });
        Ok(Self { capture: Some(capture_ep), play: Some(play_ep), rx: VecDeque::new() })
    }

    pub fn open_capture(capture: &str) -> io::Result<Self> {
        let caps = endpoints(E_CAPTURE)?;
        let cap = pick(&caps, capture, true)?;
        let capture = Endpoint::open(&cap.id, false)?;
        say!("wasapi capture {} ({})", cap.name, if capture.channels == 1 { "mono" } else { "stereo" });
        Ok(Self { capture: Some(capture), play: None, rx: VecDeque::new() })
    }

    fn pump(&mut self) -> io::Result<()> {
        if let Some(c) = &self.capture {
            c.read_into(&mut self.rx)?;
        }
        Ok(())
    }
}

impl Audio for WasapiIo {
    fn read(&mut self, x: &mut [i16]) -> io::Result<()> {
        let until = Instant::now() + IO_TIMEOUT;
        while self.rx.len() < x.len() {
            self.pump()?;
            if self.rx.len() >= x.len() {
                break;
            }
            let wait = super::remaining(until, "wasapi capture")?;
            thread::sleep(wait.min(std::time::Duration::from_millis(2)));
        }
        for s in x {
            *s = self.rx.pop_front().unwrap_or(0);
        }
        Ok(())
    }

    fn write(&mut self, x: &[i16]) -> io::Result<()> {
        let until = Instant::now() + IO_TIMEOUT;
        let mut off = 0;
        while off < x.len() {
            let room = {
                let play = self.play.as_ref().ok_or_else(capture_only)?;
                play.capacity as usize - play.padding()?.min(play.capacity as usize)
            };
            if room == 0 {
                let wait = super::remaining(until, "wasapi playback")?;
                thread::sleep(wait.min(std::time::Duration::from_millis(2)));
                continue;
            }
            let n = room.min(x.len() - off);
            self.play.as_ref().ok_or_else(capture_only)?.write_frames(&x[off..off + n])?;
            off += n;
        }
        Ok(())
    }

    fn queued(&mut self) -> io::Result<Option<Queue>> {
        let Some(play) = &self.play else { return Ok(None) };
        Ok(Some(Queue { frames: play.padding()?, floor: 0, underruns: 0 }))
    }

    fn backlog(&mut self) -> io::Result<Option<usize>> {
        self.pump()?;
        Ok(self.capture.as_ref().map(|_| self.rx.len()))
    }

    fn path(&mut self) -> io::Result<Path> {
        Ok(Path {
            capture: self.capture.as_ref().map(|c| c.latency),
            keying: 0,
            out: self.play.as_ref().map(|p| p.latency).unwrap_or(0),
            queue: self.play.is_some(),
        })
    }
}

pub fn list() -> io::Result<(DeviceList, DeviceList)> {
    let show = |d: &Dev| (d.id.clone(), d.name.clone());
    Ok((endpoints(E_CAPTURE)?.iter().map(show).collect(), endpoints(E_RENDER)?.iter().map(show).collect()))
}

pub fn check(playback: &str, radio: bool) -> io::Result<()> {
    let plays = endpoints(E_RENDER)?;
    let play = pick(&plays, playback, false)?;
    if radio || virtual_playback_name(&play.name) {
        return Ok(());
    }
    Err(not_a_cable(&play.name))
}

fn not_a_cable(name: &str) -> io::Error {
    io::Error::other(format!(
        "playback {name} is not a virtual cable. \
         use a name containing cable, virtual, voicemeeter, or null, or pass --radio"
    ))
}
