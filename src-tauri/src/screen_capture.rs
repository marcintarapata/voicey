//! One-frame screen capture for the grounded dictation binding (D4/D5).
//!
//! [`capture_primary_display`] grabs exactly one frame of the primary
//! display at dictation stop — the screen the words referred to. The frame
//! is [`ScreenFrame`]: raw RGBA in memory, no files, no clipboard, and the
//! caller drops it as soon as the vision model has consumed it. History
//! keeps text, never pixels.
//!
//! Windows: Windows Graphics Capture first (sharp, color-correct, sees most
//! content), falling back to a GDI `BitBlt` of the whole virtual screen when
//! WGC is unavailable or fails — older Windows, a refused WinRT activation,
//! or capture-blocked content. What GDI cannot see either (DRM video) is
//! black by OS design; grounding then simply has less to read.
//!
//! The seam is deliberately this small — one struct, one function — so the
//! capture path stays mockable: everything downstream of
//! [`ScreenFrame::new`] is plain data and unit-testable without a display.
//!
//! Dev builds may persist captures for quality inspection (D10): a
//! compile-time `debug_assertions` block writes each frame as PNG under
//! `Data/logs/frames/`. Release builds compile no persistence at all —
//! capture stays 100% in memory.

use std::fmt;

/// One captured frame: raw RGBA8, row-major, top-down, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ScreenFrame {
    /// A frame from raw parts. `None` unless the buffer is exactly
    /// `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        (rgba.len() == width as usize * height as usize * 4).then_some(Self {
            width,
            height,
            rgba,
        })
    }
}

#[derive(Debug)]
pub enum CaptureError {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    /// Neither Windows Graphics Capture nor the GDI fallback produced a
    /// frame.
    Windows(String),
    #[cfg(target_os = "linux")]
    Linux(String),
    #[allow(dead_code)]
    Unsupported,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
            CaptureError::Windows(detail) => write!(f, "screen capture failed: {detail}"),
            #[cfg(target_os = "linux")]
            CaptureError::Linux(detail) => write!(f, "screen capture failed: {detail}"),
            CaptureError::Unsupported => {
                f.write_str("screen capture is not built for this platform")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

/// Capture one frame of the primary display, in memory only.
pub fn capture_primary_display() -> Result<ScreenFrame, CaptureError> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        let frame = match windows_graphics_capture::capture() {
            Ok(frame) => frame,
            Err(e) => {
                log::warn!("Windows Graphics Capture failed; trying GDI: {e}");
                gdi_capture()?
            }
        };
        dev_persist_frame(&frame);
        Ok(frame)
    }
    #[cfg(all(target_os = "windows", not(target_arch = "x86_64")))]
    {
        return Err(CaptureError::Unsupported);
    }
    #[cfg(target_os = "linux")]
    {
        let frame = linux_capture().map_err(CaptureError::Linux)?;
        dev_persist_frame(&frame);
        Ok(frame)
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )))]
    {
        Err(CaptureError::Unsupported)
    }
}

// --- Windows: Windows Graphics Capture -------------------------------------

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
mod windows_graphics_capture {
    use super::{CaptureError, ScreenFrame};
    use std::sync::mpsc;
    use std::time::Duration;

    /// How long to wait for the first frame before treating capture as failed
    /// (the GDI fallback then takes over).
    const FRAME_DEADLINE: Duration = Duration::from_secs(2);

    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    /// Delivers the first frame, then stops the session. The cursor is part
    /// of the evidence (a verbal "this here" points where the cursor sits);
    /// the yellow capture border is not.
    struct FirstFrame {
        done: mpsc::Sender<ScreenFrame>,
    }

    impl GraphicsCaptureApiHandler for FirstFrame {
        type Flags = mpsc::Sender<ScreenFrame>;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { done: ctx.flags })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            capture_control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let buffer = frame.buffer()?;
            let width = buffer.width();
            let height = buffer.height();
            let mut raw = Vec::new();
            let pixels = buffer.as_nopadding_buffer(&mut raw);
            let rgba = pixels.to_vec();
            if let Some(frame) = ScreenFrame::new(width, height, rgba) {
                let _ = self.done.send(frame);
            }
            // One frame is the contract (D4); stop no matter what the
            // conversion said, so the session never lingers.
            capture_control.stop();
            Ok(())
        }
    }

    /// Capture the primary monitor through Windows Graphics Capture.
    pub(super) fn capture() -> Result<ScreenFrame, CaptureError> {
        let monitor = Monitor::primary().map_err(|e| CaptureError::Windows(format!("{e}")))?;
        let (done_tx, done_rx) = mpsc::channel::<ScreenFrame>();
        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            // The delivery channel rides in as the handler's flags.
            done_tx,
        );
        let control = FirstFrame::start_free_threaded(settings)
            .map_err(|e| CaptureError::Windows(format!("{e}")))?;
        let frame = done_rx
            .recv_timeout(FRAME_DEADLINE)
            .map_err(|_| CaptureError::Windows("no frame arrived in time".into()))?;
        // The handler already stopped the capture loop; this joins the
        // capture thread so nothing outlives the call.
        let _ = control.stop();
        Ok(frame)
    }
}

// --- Windows: GDI fallback ---------------------------------------------------

/// Capture the whole virtual screen with GDI: the oldest, most portable
/// path. `CAPTUREBLT` includes layered windows. Content GDI cannot read
/// (DRM-protected video) comes back black, which is the OS's rule, not a
/// failure of ours — grounding just has less to read.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn gdi_capture() -> Result<ScreenFrame, CaptureError> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    // SAFETY: every GDI call below takes handles returned by the calls that
    // precede it; nothing escapes this function.
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if width <= 0 || height <= 0 {
            return Err(CaptureError::Windows("no virtual screen".into()));
        }
        let (width, height) = (width as u32, height as u32);

        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err(CaptureError::Windows("GetDC failed".into()));
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            let _ = ReleaseDC(None, screen_dc);
            return Err(CaptureError::Windows("CreateCompatibleDC failed".into()));
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width as i32, height as i32);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
            return Err(CaptureError::Windows(
                "CreateCompatibleBitmap failed".into(),
            ));
        }
        let old_object = SelectObject(mem_dc, bitmap.into());

        let blit = BitBlt(
            mem_dc,
            0,
            0,
            width as i32,
            height as i32,
            Some(screen_dc),
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        );
        let frame = if blit.is_err() {
            Err(CaptureError::Windows("BitBlt failed".into()))
        } else {
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    // Negative height: a top-down bitmap, row-major from the
                    // top of the screen, matching ScreenFrame's contract.
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; width as usize * height as usize * 4];
            let lines = GetDIBits(
                mem_dc,
                bitmap,
                0,
                height,
                Some(bgra.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            );
            if lines as u32 != height {
                Err(CaptureError::Windows("GetDIBits failed".into()))
            } else {
                bgra_to_rgba_in_place(&mut bgra);
                ScreenFrame::new(width, height, bgra)
                    .ok_or_else(|| CaptureError::Windows("frame size mismatch".into()))
            }
        };

        // Return every GDI object, whatever the outcome above.
        SelectObject(mem_dc, old_object);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);
        frame
    }
}

/// GDI hands back BGRA; [`ScreenFrame`] carries RGBA. Swapped in place.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn bgra_to_rgba_in_place(bgra: &mut [u8]) {
    // SAFETY-free slice access through as_chunks; the input length is a
    // whole number of pixels by construction (width * height * 4).
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

// --- Linux: xcap --------------------------------------------------------------

#[cfg(target_os = "linux")]
fn linux_capture() -> Result<ScreenFrame, String> {
    use xcap::Monitor;
    let monitors = Monitor::all().map_err(|e| format!("listing monitors failed: {e}"))?;
    let primary = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| {
            // Fall back to the first monitor the compositor lists.
            Monitor::all().ok().and_then(|all| all.into_iter().next())
        })
        .ok_or_else(|| "no monitor found".to_string())?;
    let image = primary
        .capture_image()
        .map_err(|e| format!("capture failed: {e}"))?;
    let width = image.width();
    let height = image.height();
    let rgba = image.into_raw();
    ScreenFrame::new(width, height, rgba).ok_or_else(|| "unexpected frame size".to_string())
}

// --- Dev-only telemetry (D10) -------------------------------------------------

/// Persist a capture for quality inspection. Compiled ONLY into debug
/// builds: release builds must capture nothing (a build-time rule, never a
/// runtime setting). Frames land under `Data/logs/frames/`, outside the
/// history database — history keeps text, never pixels.
#[cfg(all(
    debug_assertions,
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )
))]
fn dev_persist_frame(frame: &ScreenFrame) {
    use image::ImageFormat;
    use std::io::Cursor;

    let Some(dir) = crate::portable::data_dir() else {
        log::debug!("dev telemetry: no data dir; frame not persisted");
        return;
    };
    let dir = dir.join("logs").join("frames");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("dev telemetry: could not create {}: {e}", dir.display());
        return;
    }
    let path = dir.join(format!(
        "{}.png",
        chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f")
    ));
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
        .expect("ScreenFrame buffers are exact");
    let mut bytes = Cursor::new(Vec::new());
    if let Err(e) = image.write_to(&mut bytes, ImageFormat::Png) {
        log::warn!("dev telemetry: PNG encode failed: {e}");
        return;
    }
    match std::fs::write(&path, bytes.into_inner()) {
        Ok(()) => log::debug!("dev telemetry: persisted {}", path.display()),
        Err(e) => log::warn!("dev telemetry: could not write {}: {e}", path.display()),
    }
}

#[cfg(not(all(
    debug_assertions,
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )
)))]
fn dev_persist_frame(_frame: &ScreenFrame) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_needs_exactly_four_bytes_per_pixel() {
        assert!(ScreenFrame::new(2, 2, vec![0; 16]).is_some());
        assert!(ScreenFrame::new(2, 2, vec![0; 15]).is_none());
        assert!(ScreenFrame::new(2, 2, vec![0; 17]).is_none());
        assert!(ScreenFrame::new(0, 0, Vec::new()).is_some());
    }

    /// The real capture path against the live session's primary display.
    /// Ignored by default — it needs an interactive desktop (the bridge
    /// smoke's session). Run with:
    /// `cargo test --lib screen_capture -- --ignored --nocapture`
    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    ))]
    #[test]
    #[ignore = "needs an interactive desktop session"]
    fn captures_a_frame_from_the_primary_display() {
        let frame = capture_primary_display().expect("capture the primary display");
        println!(
            "captured {}x{} ({} bytes)",
            frame.width,
            frame.height,
            frame.rgba.len()
        );
        assert!(frame.width >= 640, "implausibly small display");
        assert!(frame.height >= 480, "implausibly small display");
        // A real screen is never one flat color; sample two distant pixels
        // that would differ on any desktop with content.
        let stride = frame.width as usize * 4;
        let mid = frame.height as usize / 2 * stride + stride / 2;
        let top = stride / 2;
        assert!(
            frame.rgba[top..top + 3] != frame.rgba[mid..mid + 3]
                || frame.rgba.iter().any(|&b| b != frame.rgba[0]),
            "frame looks like a single flat color; suspicious for a live display"
        );
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    #[test]
    fn gdi_bgra_becomes_rgba_in_place() {
        // BGRA blue: B=255, G=0, R=0, A=255 → RGBA red: R=0, G=0, B=255.
        let mut bytes = [255u8, 0, 0, 255, 1, 2, 3, 4];
        bgra_to_rgba_in_place(&mut bytes);
        assert_eq!(bytes, [0u8, 0, 255, 255, 3, 2, 1, 4]);
    }
}
