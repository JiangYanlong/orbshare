use crate::{
    AudioFrame, CaptureConfig, CaptureProvider, CaptureSource, CaptureStream, EncodedFrame,
};
use std::collections::HashMap;
use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, OnceLock};
use std::thread;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use windows::core::{Interface, HRESULT, HSTRING};
use windows::Win32::Foundation::{BOOL, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIResource,
    DXGI_ADAPTER_DESC1, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::System::WinRT::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDevice};

const CHANNEL_BUFFER: usize = 16;
const E_APPLICATION_EXITING: HRESULT = HRESULT(0x8000001Au32 as i32);

// ── Windows Graphics Capture COM GUIDs ───────────────────────────────────

static CLSID_GRAPHICS_CAPTURE_ITEM_INTEROP: windows::core::GUID =
    windows::core::GUID::from_u128(0x362a2afe_423a_5b2e_9565_c1121c588013);

static CLSID_DIRECT3D11_CAPTURE_FRAME_POOL_STATIC2: windows::core::GUID =
    windows::core::GUID::from_u128(0x038f89d3_016a_5624_8dc4_99962299477c);

// ── COM vtable definitions ───────────────────────────────────────────────

type IUnknownQueryInterface =
    unsafe extern "system" fn(*mut c_void, *const windows::core::GUID, *mut *mut c_void) -> HRESULT;
type IUnknownAddRef = unsafe extern "system" fn(*mut c_void) -> u32;
type IUnknownRelease = unsafe extern "system" fn(*mut c_void) -> u32;

type CreateForMonitor = unsafe extern "system" fn(
    *mut c_void,
    isize,
    *const windows::core::GUID,
    *mut *mut c_void,
) -> HRESULT;

#[repr(C)]
struct IGraphicsCaptureItemInteropVtable {
    query_interface: IUnknownQueryInterface,
    add_ref: IUnknownAddRef,
    release: IUnknownRelease,
    create_for_monitor: CreateForMonitor,
}

type CreateFreeThreaded = unsafe extern "system" fn(
    *mut c_void,
    u32,
    u32,
    DXGI_FORMAT,
    u32,
    *const windows::core::GUID,
    *mut *mut c_void,
) -> HRESULT;

#[repr(C)]
struct IDirect3D11CaptureFramePoolStatics2Vtable {
    query_interface: IUnknownQueryInterface,
    add_ref: IUnknownAddRef,
    release: IUnknownRelease,
    create_free_threaded: CreateFreeThreaded,
}

type TryGetNextFrame =
    unsafe extern "system" fn(*mut c_void, *const windows::core::GUID, *mut *mut c_void) -> HRESULT;

#[repr(C)]
struct IDirect3D11CaptureFramePoolVtable {
    query_interface: IUnknownQueryInterface,
    add_ref: IUnknownAddRef,
    release: IUnknownRelease,
    // IActivationFactory::ActivateInstance
    activate_instance: *mut c_void,
    // IDirect3D11CaptureFramePool methods
    create_capture_frame: *mut c_void,
    get_capture_frame_size: *mut c_void,
    get_pixel_format: *mut c_void,
    get_dispatcher_queue: *mut c_void,
    try_get_next_frame: TryGetNextFrame,
}

#[repr(C)]
struct IGraphicsCaptureSessionVtable {
    query_interface: IUnknownQueryInterface,
    add_ref: IUnknownAddRef,
    release: IUnknownRelease,
    // IGraphicsCaptureSession methods
    start_capture: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

#[repr(C)]
struct IDirect3D11CaptureFrameVtable {
    query_interface: IUnknownQueryInterface,
    add_ref: IUnknownAddRef,
    release: IUnknownRelease,
    // IDirect3D11CaptureFrame methods
    get_surface: unsafe extern "system" fn(
        *mut c_void,
        *const windows::core::GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    get_system_relative_time: *mut c_void,
    get_content_size: *mut c_void,
    close: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

#[repr(C)]
struct IDirect3DDxgiInterfaceAccessVtable {
    query_interface: IUnknownQueryInterface,
    add_ref: IUnknownAddRef,
    release: IUnknownRelease,
    // IDirect3DDxgiInterfaceAccess methods
    get_interface: unsafe extern "system" fn(
        *mut c_void,
        *const windows::core::GUID,
        *mut *mut c_void,
    ) -> HRESULT,
}

// ── COM helpers ──────────────────────────────────────────────────────────

fn get_activation_factory(
    class_name: &HSTRING,
    iid: &windows::core::GUID,
) -> windows::core::Result<*mut c_void> {
    unsafe {
        let mut factory_ptr: *mut c_void = ptr::null_mut();
        let hr = windows::Win32::System::WinRT::RoGetActivationFactory(
            class_name.clone(),
            iid,
            &mut factory_ptr as *mut *mut c_void,
        );
        if hr.is_ok() {
            Ok(factory_ptr)
        } else {
            Err(hr.into())
        }
    }
}

unsafe fn create_for_monitor(
    interop_ptr: *mut c_void,
    hmonitor: isize,
    result_iid: &windows::core::GUID,
) -> windows::core::Result<*mut c_void> {
    let vtable = *(interop_ptr as *const *const IGraphicsCaptureItemInteropVtable);
    let mut result_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*vtable).create_for_monitor)(interop_ptr, hmonitor, result_iid, &mut result_ptr);
    if hr.is_ok() {
        Ok(result_ptr)
    } else {
        Err(hr.into())
    }
}

unsafe fn create_free_threaded_frame_pool(
    statics_ptr: *mut c_void,
    pixel_format: u32,
    number_of_buffers: u32,
    size: u32,
    result_iid: &windows::core::GUID,
) -> windows::core::Result<*mut c_void> {
    let vtable = *(statics_ptr as *const *const IDirect3D11CaptureFramePoolStatics2Vtable);
    let mut result_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*vtable).create_free_threaded)(
        statics_ptr,
        pixel_format,
        size >> 16,    // width  (high 16 bits)
        size & 0xFFFF, // height (low 16 bits)
        number_of_buffers,
        result_iid,
        &mut result_ptr,
    );
    if hr.is_ok() {
        Ok(result_ptr)
    } else {
        Err(hr.into())
    }
}

unsafe fn try_get_next_frame(pool_ptr: *mut c_void) -> windows::core::Result<*mut c_void> {
    let vtable = *(pool_ptr as *const *const IDirect3D11CaptureFramePoolVtable);
    let mut frame_ptr: *mut c_void = ptr::null_mut();
    let hr =
        ((*vtable).try_get_next_frame)(pool_ptr, &IDirect3D11CaptureFrame::IID, &mut frame_ptr);
    if hr.is_ok() {
        Ok(frame_ptr)
    } else {
        Err(hr.into())
    }
}

unsafe fn start_session(session_ptr: *mut c_void) -> windows::core::Result<()> {
    let vtable = *(session_ptr as *const *const IGraphicsCaptureSessionVtable);
    ((*vtable).start_capture)(session_ptr).ok()
}

unsafe fn get_frame_surface(frame_ptr: *mut c_void) -> windows::core::Result<*mut c_void> {
    let vtable = *(frame_ptr as *const *const IDirect3D11CaptureFrameVtable);
    let mut surface_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*vtable).get_surface)(
        frame_ptr,
        &IDirect3DDxgiInterfaceAccess::IID,
        &mut surface_ptr,
    );
    if hr.is_ok() {
        Ok(surface_ptr)
    } else {
        Err(hr.into())
    }
}

unsafe fn get_dxgi_interface(
    surface_ptr: *mut c_void,
    iid: &windows::core::GUID,
) -> windows::core::Result<*mut c_void> {
    let vtable = *(surface_ptr as *const *const IDirect3DDxgiInterfaceAccessVtable);
    let mut interface_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*vtable).get_interface)(surface_ptr, iid, &mut interface_ptr);
    if hr.is_ok() {
        Ok(interface_ptr)
    } else {
        Err(hr.into())
    }
}

unsafe fn release_com(ptr: *mut c_void) {
    if !ptr.is_null() {
        let vtable = *(ptr as *const *const IGraphicsCaptureItemInteropVtable);
        ((*vtable).release)(ptr);
    }
}

// ── WinRT interface stubs ────────────────────────────────────────────────

#[allow(non_upper_case_globals)]
mod IDirect3D11CaptureFrame {
    use windows::core::GUID;
    pub const IID: GUID = GUID::from_u128(0xcafc051f_4311_4e05_b711_e410029f3a44);
}

#[allow(non_upper_case_globals)]
mod IDirect3DDxgiInterfaceAccess {
    use windows::core::GUID;
    pub const IID: GUID = GUID::from_u128(0xa9b3d012_3df2_4ee3_b8d1_8695f457d3c1);
}

// ── Capture state ────────────────────────────────────────────────────────

struct CaptureState {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging_texture: ID3D11Texture2D,
    video_tx: mpsc::Sender<EncodedFrame>,
    width: AtomicU32,
    height: AtomicU32,
    frame_count: AtomicU64,
    stop_rx: std_mpsc::Receiver<()>,
}

// ── Capture source cache for HMONITOR lookup ─────────────────────────────

static MONITOR_CACHE: OnceLock<std::sync::Mutex<HashMap<u32, isize>>> = OnceLock::new();

fn monitor_cache() -> &'static std::sync::Mutex<HashMap<u32, isize>> {
    MONITOR_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

// ── Provider ─────────────────────────────────────────────────────────────

pub struct WindowsCaptureProvider {
    capture_thread: Option<thread::JoinHandle<()>>,
    stop_tx: Option<std_mpsc::Sender<()>>,
}

impl WindowsCaptureProvider {
    pub fn new() -> Self {
        Self {
            capture_thread: None,
            stop_tx: None,
        }
    }
}

impl Default for WindowsCaptureProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureProvider for WindowsCaptureProvider {
    fn request_permission(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
        Box::pin(async move {
            // WGC doesn't require explicit permission on Windows 10 1903+.
            // The API is available without user consent for the current process.
            // A border/indicator is shown around the captured display.
            info!("[Windows] WGC capture permission auto-granted (border indicator shown)");
            Ok(true)
        })
    }

    fn start_capture(&mut self, config: CaptureConfig) -> Result<CaptureStream, String> {
        info!(
            "[Windows] Starting WGC capture: {}x{} @ {}fps",
            config.width, config.height, config.fps
        );

        let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(CHANNEL_BUFFER);
        let (stop_tx, stop_rx) = std_mpsc::channel();

        self.stop_tx = Some(stop_tx);

        let handle = thread::spawn(move || {
            if let Err(e) = run_wgc_capture(config, video_tx, stop_rx) {
                error!("[Windows] WGC capture error: {}", e);
            }
        });

        self.capture_thread = Some(handle);
        info!("[Windows] WGC capture thread started");

        Ok(CaptureStream {
            video_rx,
            encoded_video_rx: None,
            preview_video_rx: None,
            audio_rx: None,
        })
    }

    fn stop_capture(&mut self) -> Result<(), String> {
        info!("[Windows] Stopping WGC capture");
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.capture_thread.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn list_sources(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CaptureSource>, String>> + Send + '_>> {
        Box::pin(async move {
            info!("[Windows] Listing displays via DXGI");
            let displays = enumerate_displays()?;
            let mut cache = monitor_cache().lock().unwrap();
            cache.clear();
            for d in &displays {
                cache.insert(d.id, d.hmonitor);
            }
            Ok(displays.into_iter().map(|d| d.source).collect())
        })
    }
}

impl Drop for WindowsCaptureProvider {
    fn drop(&mut self) {
        let _ = self.stop_capture();
    }
}

// ── Display enumeration ──────────────────────────────────────────────────

struct DisplayInfo {
    source: CaptureSource,
    hmonitor: isize,
}

fn enumerate_displays() -> Result<Vec<DisplayInfo>, String> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1 failed: {e}"))?;
        let mut displays = Vec::new();
        let mut adapter_idx = 0u32;

        loop {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(adapter_idx) {
                Ok(a) => a,
                Err(_) => break,
            };
            adapter_idx += 1;

            let desc: DXGI_ADAPTER_DESC1 = adapter
                .GetDesc1()
                .map_err(|e| format!("GetDesc1 failed: {e}"))?;

            let adapter_name = String::from_utf16_lossy(
                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(64)],
            );

            let mut output_idx = 0u32;
            loop {
                let output: IDXGIOutput = match adapter.EnumOutputs(output_idx) {
                    Ok(o) => o,
                    Err(_) => break,
                };
                output_idx += 1;

                let out_desc = output
                    .GetDesc()
                    .map_err(|e| format!("GetDesc failed: {e}"))?;

                let w =
                    (out_desc.DesktopCoordinates.right - out_desc.DesktopCoordinates.left) as u32;
                let h =
                    (out_desc.DesktopCoordinates.bottom - out_desc.DesktopCoordinates.top) as u32;
                let hmonitor = out_desc.Monitor.0 as isize;

                let name = if output_idx == 1 {
                    format!("{} (Display {})", adapter_name, adapter_idx)
                } else {
                    format!("{} (Display {}.{})", adapter_name, adapter_idx, output_idx)
                };

                displays.push(DisplayInfo {
                    source: CaptureSource {
                        id: (adapter_idx - 1) * 8 + output_idx - 1,
                        name,
                        width: w,
                        height: h,
                        is_display: true,
                        thumbnail_data_url: None,
                    },
                    hmonitor,
                });
            }
        }

        if displays.is_empty() {
            return Err("No displays found via DXGI".into());
        }

        Ok(displays)
    }
}

// ── WGC capture loop ─────────────────────────────────────────────────────

fn run_wgc_capture(
    config: CaptureConfig,
    video_tx: mpsc::Sender<EncodedFrame>,
    stop_rx: std_mpsc::Receiver<()>,
) -> Result<(), String> {
    unsafe {
        // 1. Create D3D11 device
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL(0xb000)]), // D3D_FEATURE_LEVEL_11_0
            1,                                  // D3D11_SDK_VERSION
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice failed: {e}"))?;
        let device = device.ok_or("D3D11CreateDevice returned null device")?;
        let context = context.ok_or("D3D11CreateDevice returned null context")?;
        info!("[Windows] D3D11 device created");

        // 2. Get DXGI device and adapter
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|e| format!("QueryInterface<IDXGIDevice> failed: {e}"))?;
        let adapter: IDXGIAdapter1 = dxgi_device
            .GetParent()
            .map_err(|e| format!("GetParent<IDXGIAdapter1> failed: {e}"))?;

        // 3. Create WinRT IDirect3DDevice
        let d3d_device_winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
            .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice failed: {e}"))?;
        let d3d_device_raw = d3d_device_winrt.as_raw();

        // 4. Enumerate displays and find target
        let mut target_hmonitor: isize = 0;
        let mut output_width: u32 = 0;
        let mut output_height: u32 = 0;
        let mut output_idx = 0u32;

        loop {
            let output: IDXGIOutput = match adapter.EnumOutputs(output_idx) {
                Ok(o) => o,
                Err(_) => break,
            };
            output_idx += 1;

            let out_desc = output
                .GetDesc()
                .map_err(|e| format!("GetDesc failed: {e}"))?;

            let w = (out_desc.DesktopCoordinates.right - out_desc.DesktopCoordinates.left) as u32;
            let h = (out_desc.DesktopCoordinates.bottom - out_desc.DesktopCoordinates.top) as u32;

            if target_hmonitor == 0 {
                // Use the first (primary) display
                target_hmonitor = out_desc.Monitor.0 as isize;
                output_width = w;
                output_height = h;
                info!("[Windows] Primary display: {}x{}", w, h);
            }
        }

        if target_hmonitor == 0 {
            return Err("No display found for capture".into());
        }

        // 5. Get IGraphicsCaptureItemInterop activation factory
        let class_name = HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureItem");
        let interop_ptr = get_activation_factory(&class_name, &CLSID_GRAPHICS_CAPTURE_ITEM_INTEROP)
            .map_err(|e| format!("Get GraphicsCaptureItemInterop failed: {e}"))?;

        // 6. Create capture item for the target monitor
        let item_ptr = create_for_monitor(
            interop_ptr,
            target_hmonitor,
            &windows::core::IID, // IGraphicsCaptureItem
        )
        .map_err(|e| format!("CreateForMonitor failed: {e}"))?;
        info!("[Windows] Capture item created for monitor");

        // 7. Create staging texture for CPU readback
        let mut staging_desc = D3D11_TEXTURE2D_DESC::default();
        staging_desc.Width = output_width;
        staging_desc.Height = output_height;
        staging_desc.MipLevels = 1;
        staging_desc.ArraySize = 1;
        staging_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        staging_desc.SampleDesc.Count = 1;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;

        let mut staging_texture: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))
            .map_err(|e| format!("CreateTexture2D (staging) failed: {e}"))?;
        let staging_texture = staging_texture.ok_or("CreateTexture2D returned null")?;

        // 8. Get IDirect3D11CaptureFramePoolStatics2
        let pool_class = HSTRING::from("Windows.Graphics.Capture.Direct3D11CaptureFramePool");
        let pool_statics_ptr =
            get_activation_factory(&pool_class, &CLSID_DIRECT3D11_CAPTURE_FRAME_POOL_STATIC2)
                .map_err(|e| format!("Get CaptureFramePoolStatics2 failed: {e}"))?;

        // 9. Create frame pool (free-threaded, supports polling)
        let pool_ptr = create_free_threaded_frame_pool(
            pool_statics_ptr,
            DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            1,                                    // number of buffers
            (output_width << 16) | output_height, // packed size
            &windows::core::IID,
        )
        .map_err(|e| format!("CreateFreeThreaded frame pool failed: {e}"))?;
        info!("[Windows] Frame pool created");

        // 10. Create capture session
        let session_class = HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession");
        let session_factory_ptr = get_activation_factory(&session_class, &windows::core::IID)
            .map_err(|e| format!("Get GraphicsCaptureSession factory failed: {e}"))?;

        let mut session_ptr: *mut c_void = ptr::null_mut();
        let vtable = *(session_factory_ptr as *const *const IGraphicsCaptureSessionVtable);
        let activate_instance: *mut c_void =
            *((session_factory_ptr as *const *const c_void).offset(3)); // IActivationFactory::ActivateInstance at vtable[3]
        let hr = std::mem::transmute::<
            _,
            unsafe extern "system" fn(
                *mut c_void,
                *const windows::core::GUID,
                *mut *mut c_void,
            ) -> HRESULT,
        >(activate_instance)(
            session_factory_ptr, &windows::core::IID, &mut session_ptr
        );
        if !hr.is_ok() {
            return Err(format!("ActivateInstance for session failed: {hr:?}"));
        }
        info!("[Windows] Capture session created");

        // 11. Start capture
        start_session(session_ptr).map_err(|e| format!("StartCapture failed: {e}"))?;
        info!("[Windows] WGC capture started");

        // Store stop signal sender in CaptureState
        let state = Box::new(CaptureState {
            device: device.clone(),
            context: context.clone(),
            staging_texture: staging_texture.clone(),
            video_tx,
            width: AtomicU32::new(output_width),
            height: AtomicU32::new(output_height),
            frame_count: AtomicU64::new(0),
            stop_rx,
        });

        // 12. Capture loop (polling)
        let state_ref = &*state;
        let mut running = true;

        while running {
            // Check stop signal (non-blocking)
            match state_ref.stop_rx.try_recv() {
                Ok(()) | Err(std_mpsc::TryRecvError::Disconnected) => {
                    info!("[Windows] Stop signal received");
                    running = false;
                    break;
                }
                Err(std_mpsc::TryRecvError::Empty) => {}
            }

            // Try to get next frame
            match try_get_next_frame(pool_ptr) {
                Ok(frame_ptr) => {
                    // Got a frame
                    let mut tex_desc = D3D11_TEXTURE2D_DESC::default();
                    tex_desc.Width = state_ref.width.load(Ordering::Relaxed);
                    tex_desc.Height = state_ref.height.load(Ordering::Relaxed);

                    let surface_ptr = get_frame_surface(frame_ptr)
                        .map_err(|e| format!("GetSurface failed: {e}"))?;

                    let tex2d_ptr = get_dxgi_interface(surface_ptr, &ID3D11Texture2D::IID)
                        .map_err(|e| format!("GetInterface<ID3D11Texture2D> failed: {e}"))?;

                    // SAFETY: tex2d_ptr is a valid ID3D11Texture2D
                    let src_texture = std::mem::transmute::<_, ID3D11Texture2D>(tex2d_ptr);

                    // Get dimensions from source texture
                    src_texture.GetDesc(&mut tex_desc);
                    state_ref.width.store(tex_desc.Width, Ordering::Relaxed);
                    state_ref.height.store(tex_desc.Height, Ordering::Relaxed);

                    // Copy to staging texture
                    state_ref
                        .context
                        .CopyResource(&state_ref.staging_texture, &src_texture);

                    // Map staging texture
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    state_ref
                        .context
                        .Map(
                            &state_ref.staging_texture,
                            0,
                            D3D11_MAP_READ,
                            0,
                            Some(&mut mapped),
                        )
                        .map_err(|e| format!("Map failed: {e}"))?;

                    let width = tex_desc.Width as usize;
                    let height = tex_desc.Height as usize;
                    let stride = mapped.RowPitch as usize;
                    let src_slice =
                        std::slice::from_raw_parts(mapped.pData as *const u8, stride * height);

                    // BGRA → YUV420 (Windows: top-left origin, no vertical flip needed)
                    let y_size = width * height;
                    let uv_w = width / 2;
                    let uv_h = height / 2;
                    let uv_size = uv_w * uv_h;
                    let mut yuv = vec![0u8; y_size + uv_size * 2];

                    for row in 0..height {
                        for col in 0..width {
                            let src_off = row * stride + col * 4;
                            let b = src_slice[src_off] as f32;
                            let g = src_slice[src_off + 1] as f32;
                            let r = src_slice[src_off + 2] as f32;

                            let y = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
                            let u_val = (-0.169 * r - 0.331 * g + 0.500 * b + 128.0)
                                .clamp(0.0, 255.0) as u8;
                            let v_val =
                                (0.500 * r - 0.419 * g - 0.081 * b + 128.0).clamp(0.0, 255.0) as u8;

                            yuv[row * width + col] = y;

                            if row % 2 == 0 && col % 2 == 0 {
                                let uv_idx = y_size + (row / 2) * uv_w + (col / 2);
                                yuv[uv_idx] = u_val;
                                yuv[uv_idx + uv_size] = v_val;
                            }
                        }
                    }

                    state_ref.context.Unmap(&state_ref.staging_texture, 0);

                    let frame_num = state_ref.frame_count.fetch_add(1, Ordering::Relaxed);
                    let is_keyframe = frame_num % 120 == 0;

                    let frame = EncodedFrame {
                        data: yuv,
                        timestamp_us: frame_num * 16_667, // ~60fps
                        is_keyframe,
                        width: width as u32,
                        height: height as u32,
                    };

                    // Log periodically
                    if frame_num % 60 == 0 {
                        debug!(
                            "[Windows] WGC frame #{}: {}x{} ({} bytes YUV)",
                            frame_num,
                            width,
                            height,
                            frame.data.len()
                        );
                    }

                    if video_tx.try_send(frame).is_err() {
                        warn!("[Windows] Video channel full or closed, dropping frame");
                    }

                    // Release the frame (allow pool to reuse it)
                    let release_frame = *(frame_ptr as *const *const c_void).offset(2); // IUnknown::Release
                    std::mem::transmute::<_, unsafe extern "system" fn(*mut c_void) -> u32>(
                        release_frame,
                    )(frame_ptr);

                    // Release DXGI objects
                    release_com(tex2d_ptr);
                    release_com(surface_ptr);

                    // Don't sleep — go straight to next poll
                }
                Err(e) => {
                    let hr_val = format!("{:?}", e);
                    if hr_val.contains("8000001a") {
                        // E_APPLICATION_EXITING — normal exit
                        debug!("[Windows] WGC session exiting");
                        running = false;
                    } else {
                        // No frame available — brief sleep to avoid busy-wait
                        thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        }

        // Cleanup
        info!("[Windows] Cleaning up WGC capture resources");
        start_session(session_ptr).ok(); // Ensure session is stopped (no-op if already stopped)
        thread::sleep(std::time::Duration::from_millis(50));

        release_com(session_ptr);
        release_com(pool_ptr);
        release_com(item_ptr);
        release_com(interop_ptr);
        release_com(pool_statics_ptr);
        release_com(session_factory_ptr);

        let frame_count = state_ref.frame_count.load(Ordering::Relaxed);
        info!(
            "[Windows] WGC capture stopped. Total frames: {}",
            frame_count
        );

        Ok(())
    }
}
