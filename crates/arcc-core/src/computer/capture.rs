//! macOS screen capture via `xcap`.
//!
//! `Monitor` is not guaranteed to be `Send`, so every public function here is
//! blocking and must be called from `tokio::task::spawn_blocking`; it creates
//! and drops the monitor inside that closure and returns only plain data.

use std::path::PathBuf;

use base64::Engine as _;
use image::ImageEncoder;
use xcap::Monitor;

use super::{ComputerError, ScreenGeometryInfo};
use arcc_storage::config::loader::ComputerUseConfig;

pub struct CapturedScreen {
    pub info: ScreenGeometryInfo,
    pub png_base64: String,
    /// Heuristic: a fully black frame usually means Screen Recording is denied.
    pub appears_black: bool,
    pub saved_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
    pub primary: bool,
}

/// Capture one monitor (primary when `display` is None).
pub fn capture(
    display: Option<usize>,
    cfg: &ComputerUseConfig,
) -> Result<CapturedScreen, ComputerError> {
    let (index, monitor, count) = select_monitor(display)?;

    let screen_x = monitor.x().map_err(capture_err)?;
    let screen_y = monitor.y().map_err(capture_err)?;
    let screen_width = monitor.width().map_err(capture_err)?;
    let screen_height = monitor.height().map_err(capture_err)?;
    let scale_factor = monitor.scale_factor().map_err(capture_err)?.max(0.1);

    let image = monitor
        .capture_image()
        .map_err(|e| ComputerError::Capture(format!("capture_image failed: {e}")))?;
    let (orig_w, orig_h) = image.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return Err(ComputerError::Capture(
            "capture returned an empty image".into(),
        ));
    }

    let (image_width, image_height) = fit_width(orig_w, orig_h, cfg.max_screenshot_width);
    let resized = if image_width < orig_w {
        image::imageops::resize(
            &image,
            image_width,
            image_height,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };

    let appears_black = appears_black(&resized);
    let png = encode_png(&resized)?;
    let saved_path = if cfg.save_screenshots {
        save_png(&png)
    } else {
        None
    };

    Ok(CapturedScreen {
        info: ScreenGeometryInfo {
            display: index,
            display_count: count,
            screen_x,
            screen_y,
            screen_width,
            screen_height,
            image_width,
            image_height,
            scale_factor,
        },
        png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
        appears_black,
        saved_path,
    })
}

/// Query a monitor's geometry without capturing pixels.
///
/// The resulting image dimensions match what [`capture`] would produce for the
/// same monitor/config, which is what coordinate mapping needs.
pub fn geometry(
    display: Option<usize>,
    max_width: u32,
) -> Result<ScreenGeometryInfo, ComputerError> {
    let (index, monitor, count) = select_monitor(display)?;

    let screen_x = monitor.x().map_err(capture_err)?;
    let screen_y = monitor.y().map_err(capture_err)?;
    let screen_width = monitor.width().map_err(capture_err)?;
    let screen_height = monitor.height().map_err(capture_err)?;
    let scale_factor = monitor.scale_factor().map_err(capture_err)?.max(0.1);

    let physical_w = (screen_width as f32 * scale_factor).round().max(1.0) as u32;
    let physical_h = (screen_height as f32 * scale_factor).round().max(1.0) as u32;
    let (image_width, image_height) = fit_width(physical_w, physical_h, max_width);

    Ok(ScreenGeometryInfo {
        display: index,
        display_count: count,
        screen_x,
        screen_y,
        screen_width,
        screen_height,
        image_width,
        image_height,
        scale_factor,
    })
}

/// Enumerate monitors for the `status` action.
pub fn list_monitors() -> Result<Vec<MonitorInfo>, ComputerError> {
    let monitors = Monitor::all().map_err(capture_err)?;
    let mut result = Vec::with_capacity(monitors.len());
    for (index, monitor) in monitors.iter().enumerate() {
        let primary = monitor.is_primary().unwrap_or(false);
        result.push(MonitorInfo {
            index,
            name: monitor
                .friendly_name()
                .or_else(|_| monitor.name())
                .unwrap_or_else(|_| format!("display-{index}")),
            x: monitor.x().unwrap_or(0),
            y: monitor.y().unwrap_or(0),
            width: monitor.width().unwrap_or(0),
            height: monitor.height().unwrap_or(0),
            scale_factor: monitor.scale_factor().unwrap_or(1.0),
            primary,
        });
    }
    Ok(result)
}

fn select_monitor(display: Option<usize>) -> Result<(usize, Monitor, usize), ComputerError> {
    let monitors = Monitor::all().map_err(capture_err)?;
    let count = monitors.len();
    if count == 0 {
        return Err(ComputerError::Capture("no monitors detected".into()));
    }

    let index = match display {
        Some(index) if index < count => index,
        Some(index) => {
            return Err(ComputerError::InvalidArgs(format!(
                "display {index} does not exist; {count} monitor(s) available"
            )));
        }
        None => monitors
            .iter()
            .position(|m| m.is_primary().unwrap_or(false))
            .unwrap_or(0),
    };

    let monitor = monitors
        .into_iter()
        .nth(index)
        .ok_or_else(|| ComputerError::Capture(format!("display {index} not available")))?;
    Ok((index, monitor, count))
}

/// Downscale to `max_width`, preserving aspect ratio. Never upscales.
fn fit_width(width: u32, height: u32, max_width: u32) -> (u32, u32) {
    if max_width == 0 || width <= max_width {
        return (width, height);
    }
    let scale = max_width as f64 / width as f64;
    let h = (height as f64 * scale).round().max(1.0) as u32;
    (max_width, h)
}

fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>, ComputerError> {
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| ComputerError::Capture(format!("PNG encoding failed: {e}")))?;
    Ok(buf)
}

fn appears_black(image: &image::RgbaImage) -> bool {
    // Sample sparsely; enough to distinguish a real desktop from a black
    // permission-denied frame without scanning every pixel.
    let mut sampled = 0usize;
    for (i, pixel) in image.pixels().enumerate() {
        if i % 97 != 0 {
            continue;
        }
        sampled += 1;
        if pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8 {
            return false;
        }
    }
    sampled > 0
}

fn save_png(png: &[u8]) -> Option<String> {
    let home = std::env::var("ARCC_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".arcc"))
        })?;
    let dir = home.join("screenshots");
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!(
        "shot-{}.png",
        chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f")
    );
    let path = dir.join(name);
    std::fs::write(&path, png).ok()?;
    Some(path.display().to_string())
}

fn capture_err(e: impl std::fmt::Display) -> ComputerError {
    ComputerError::Capture(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::fit_width;

    #[test]
    fn fit_width_never_upscales() {
        assert_eq!(fit_width(800, 600, 1280), (800, 600));
    }

    #[test]
    fn fit_width_preserves_aspect_ratio() {
        assert_eq!(fit_width(2560, 1440, 1280), (1280, 720));
        assert_eq!(fit_width(1920, 1080, 1280), (1280, 720));
        assert_eq!(fit_width(3000, 2000, 1280), (1280, 853));
    }
}
