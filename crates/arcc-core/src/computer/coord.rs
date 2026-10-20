//! Coordinate mapping between screenshot pixels and global screen points.
//!
//! On macOS, xcap reports monitor `x/y/width/height` in logical points
//! (`CGDisplayBounds`), while `capture_image()` returns a physical-pixel image
//! (2x on Retina). The screenshot is resized to `max_screenshot_width`, so the
//! model always sees a known image size. enigo's absolute coordinates use the
//! same global point space as `CGDisplayBounds`.

use super::ScreenGeometryInfo;

/// Map a point in the latest screenshot to global screen coordinates.
///
/// Returns `(screen_x, screen_y, clamped)`; `clamped` is true when the model's
/// coordinate was outside the image or the mapped point was outside the
/// monitor bounds.
pub fn map_image_point(
    image_x: i64,
    image_y: i64,
    geometry: &ScreenGeometryInfo,
) -> (i32, i32, bool) {
    if geometry.image_width == 0 || geometry.image_height == 0 {
        return (geometry.screen_x, geometry.screen_y, true);
    }

    let max_x = geometry.image_width.saturating_sub(1) as i64;
    let max_y = geometry.image_height.saturating_sub(1) as i64;
    let cx = image_x.clamp(0, max_x);
    let cy = image_y.clamp(0, max_y);
    let mut clamped = cx != image_x || cy != image_y;

    let scale_x = geometry.screen_width as f64 / geometry.image_width as f64;
    let scale_y = geometry.screen_height as f64 / geometry.image_height as f64;

    let mut sx = geometry.screen_x + (cx as f64 * scale_x).round() as i32;
    let mut sy = geometry.screen_y + (cy as f64 * scale_y).round() as i32;

    let min_x = geometry.screen_x;
    let min_y = geometry.screen_y;
    let max_screen_x = geometry.screen_x + geometry.screen_width.saturating_sub(1) as i32;
    let max_screen_y = geometry.screen_y + geometry.screen_height.saturating_sub(1) as i32;

    if sx < min_x || sx > max_screen_x {
        sx = sx.clamp(min_x, max_screen_x);
        clamped = true;
    }
    if sy < min_y || sy > max_screen_y {
        sy = sy.clamp(min_y, max_screen_y);
        clamped = true;
    }

    (sx, sy, clamped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(
        screen_x: i32,
        screen_y: i32,
        screen_width: u32,
        screen_height: u32,
        image_width: u32,
        image_height: u32,
    ) -> ScreenGeometryInfo {
        ScreenGeometryInfo {
            display: 0,
            display_count: 1,
            screen_x,
            screen_y,
            screen_width,
            screen_height,
            image_width,
            image_height,
            scale_factor: 2.0,
        }
    }

    #[test]
    fn maps_image_pixels_to_logical_points() {
        let g = geometry(0, 0, 1440, 900, 1280, 720);
        assert_eq!(map_image_point(0, 0, &g), (0, 0, false));
        assert_eq!(map_image_point(640, 360, &g), (720, 450, false));
        assert_eq!(map_image_point(1279, 719, &g), (1439, 899, false));
    }

    #[test]
    fn retina_image_and_screen_sizes_are_independent() {
        // A 2x capture downscaled to 1280x720 still maps to 1440x900 points.
        let g = geometry(0, 0, 1440, 900, 1280, 720);
        assert_eq!(map_image_point(320, 180, &g), (360, 225, false));
    }

    #[test]
    fn applies_multi_monitor_offset() {
        let g = geometry(1440, 0, 1920, 1080, 1280, 720);
        assert_eq!(map_image_point(0, 0, &g), (1440, 0, false));
        assert_eq!(map_image_point(1279, 719, &g), (3359, 1079, false));
    }

    #[test]
    fn clamps_out_of_bounds_coordinates() {
        let g = geometry(0, 0, 1440, 900, 1280, 720);
        let (x, y, clamped) = map_image_point(-50, 9999, &g);
        assert!(clamped);
        assert_eq!((x, y), (0, 899));

        let (x, y, clamped) = map_image_point(99999, -1, &g);
        assert!(clamped);
        assert_eq!((x, y), (1439, 0));
    }

    #[test]
    fn handles_negative_monitor_origin() {
        // A secondary display placed to the left of the primary one.
        let g = geometry(-1920, 0, 1920, 1080, 1280, 720);
        assert_eq!(map_image_point(0, 0, &g), (-1920, 0, false));
        assert_eq!(map_image_point(1279, 719, &g), (-1, 1079, false));
    }
}
