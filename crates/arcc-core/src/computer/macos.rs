//! macOS implementation of the `computer` tool.

use std::time::Duration;

use enigo::Button;
use serde_json::{Value, json};
use tokio::task;

use super::capture;
use super::coord::map_image_point;
use super::input;
use super::{
    ComputerAction, ComputerArgs, ComputerError, ComputerOutput, ComputerSession,
    ScreenGeometryInfo,
};
use crate::model::types::ImageAttachment;
use arcc_storage::config::loader::ComputerUseConfig;

pub async fn execute(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    match args.action {
        ComputerAction::Screenshot => screenshot(session, args.display, cfg).await,
        ComputerAction::Status => status().await,
        ComputerAction::Click => click(session, args, cfg, button(args), 1, "click").await,
        ComputerAction::DoubleClick => {
            click(session, args, cfg, Button::Left, 2, "double_click").await
        }
        ComputerAction::RightClick => {
            click(session, args, cfg, Button::Right, 1, "right_click").await
        }
        ComputerAction::Move => move_mouse(session, args, cfg).await,
        ComputerAction::Drag => drag(session, args, cfg).await,
        ComputerAction::Scroll => scroll(session, args, cfg).await,
        ComputerAction::Type => type_text(session, args, cfg).await,
        ComputerAction::Key => press_keys(session, args, cfg).await,
        ComputerAction::Wait => wait(session, args, cfg).await,
    }
}

async fn screenshot(
    session: &mut ComputerSession,
    display: Option<usize>,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let cfg = cfg.clone();
    let shot = blocking(move || capture::capture(display, &cfg)).await?;
    session.last_screen = Some(shot.info.clone());

    let image = ImageAttachment {
        mime: "image/png".into(),
        data_base64: shot.png_base64,
        width: shot.info.image_width,
        height: shot.info.image_height,
    };

    let mut text = screenshot_json(&shot.info);
    if shot.appears_black {
        text["warning"] = json!(
            "The captured screen appears black. Check Screen Recording permission in \
             System Settings → Privacy & Security → Screen Recording."
        );
    }
    if let Some(path) = shot.saved_path {
        text["saved_path"] = json!(path);
    }

    Ok(ComputerOutput {
        text: text.to_string(),
        images: vec![image],
        screen: Some(shot.info),
    })
}

async fn status() -> Result<ComputerOutput, ComputerError> {
    let monitors = blocking(capture::list_monitors).await?;
    let cursor = blocking(|| Ok(input::status())).await?;

    let monitors: Vec<Value> = monitors
        .into_iter()
        .map(|m| {
            json!({
                "index": m.index,
                "name": m.name,
                "x": m.x,
                "y": m.y,
                "width": m.width,
                "height": m.height,
                "scale_factor": m.scale_factor,
                "primary": m.primary,
            })
        })
        .collect();

    let text = json!({
        "ok": true,
        "action": "status",
        "monitors": monitors,
        "cursor": cursor.cursor.map(|(x, y)| json!({"x": x, "y": y})),
        "main_display": cursor.main_display.map(|(w, h)| json!({"width": w, "height": h})),
    });

    Ok(ComputerOutput {
        text: text.to_string(),
        images: Vec::new(),
        screen: None,
    })
}

async fn click(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
    button: Button,
    count: u32,
    action: &str,
) -> Result<ComputerOutput, ComputerError> {
    let geometry = resolve_geometry(session, args.display, cfg.max_screenshot_width).await?;
    let (screen_x, screen_y, clamped) =
        map_image_point(args.x.unwrap_or(0), args.y.unwrap_or(0), &geometry);

    blocking(move || input::click(screen_x, screen_y, button, count)).await?;

    let details = json!({
        "ok": true,
        "action": action,
        "x": args.x,
        "y": args.y,
        "screen_x": screen_x,
        "screen_y": screen_y,
        "display": geometry.display,
        "clamped": clamped,
    });
    finish(session, args, cfg, details).await
}

async fn move_mouse(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let geometry = resolve_geometry(session, args.display, cfg.max_screenshot_width).await?;
    let (screen_x, screen_y, clamped) =
        map_image_point(args.x.unwrap_or(0), args.y.unwrap_or(0), &geometry);

    blocking(move || input::move_mouse(screen_x, screen_y)).await?;

    let details = json!({
        "ok": true,
        "action": "move",
        "x": args.x,
        "y": args.y,
        "screen_x": screen_x,
        "screen_y": screen_y,
        "display": geometry.display,
        "clamped": clamped,
    });
    finish(session, args, cfg, details).await
}

async fn drag(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let geometry = resolve_geometry(session, args.display, cfg.max_screenshot_width).await?;
    let (from_x, from_y, clamped_from) =
        map_image_point(args.x.unwrap_or(0), args.y.unwrap_or(0), &geometry);
    let (to_x, to_y, clamped_to) =
        map_image_point(args.to_x.unwrap_or(0), args.to_y.unwrap_or(0), &geometry);

    let button = button(args);
    blocking(move || input::drag(from_x, from_y, to_x, to_y, button)).await?;

    let details = json!({
        "ok": true,
        "action": "drag",
        "x": args.x,
        "y": args.y,
        "to_x": args.to_x,
        "to_y": args.to_y,
        "from_screen": {"x": from_x, "y": from_y},
        "to_screen": {"x": to_x, "y": to_y},
        "display": geometry.display,
        "clamped": clamped_from || clamped_to,
    });
    finish(session, args, cfg, details).await
}

async fn scroll(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let position = if let (Some(x), Some(y)) = (args.x, args.y) {
        let geometry = resolve_geometry(session, args.display, cfg.max_screenshot_width).await?;
        let (sx, sy, _) = map_image_point(x, y, &geometry);
        Some((sx, sy))
    } else {
        None
    };

    let direction = args.direction.clone().unwrap_or_else(|| "down".into());
    let amount = args.amount.unwrap_or(3).clamp(1, 100) as i32;
    let dir_for_input = direction.clone();
    blocking(move || input::scroll(&dir_for_input, amount, position)).await?;

    let details = json!({
        "ok": true,
        "action": "scroll",
        "direction": direction,
        "amount": amount,
        "position": position.map(|(x, y)| json!({"x": x, "y": y})),
    });
    finish(session, args, cfg, details).await
}

async fn type_text(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let text = args.text.clone().unwrap_or_default();
    let preview: String = text.chars().take(200).collect();
    let press_enter = args.press_enter.unwrap_or(false);
    let text_for_input = text.clone();
    blocking(move || input::type_text(&text_for_input, press_enter)).await?;

    let details = json!({
        "ok": true,
        "action": "type",
        "text": preview,
        "text_len": text.chars().count(),
        "press_enter": press_enter,
    });
    finish(session, args, cfg, details).await
}

async fn press_keys(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let keys = args.keys.clone().unwrap_or_default();
    let keys_for_input = keys.clone();
    blocking(move || input::press_keys(&keys_for_input)).await?;

    let details = json!({
        "ok": true,
        "action": "key",
        "keys": keys,
    });
    finish(session, args, cfg, details).await
}

async fn wait(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    let milliseconds = args.milliseconds.unwrap_or(500).min(10_000);
    tokio::time::sleep(Duration::from_millis(milliseconds)).await;

    let details = json!({
        "ok": true,
        "action": "wait",
        "milliseconds": milliseconds,
    });
    finish(session, args, cfg, details).await
}

/// Optionally capture a post-action screenshot and build the tool output.
async fn finish(
    session: &mut ComputerSession,
    args: &ComputerArgs,
    cfg: &ComputerUseConfig,
    mut details: Value,
) -> Result<ComputerOutput, ComputerError> {
    let mut images = Vec::new();

    if args.wants_screenshot(cfg.capture_after_action) {
        if cfg.action_settle_ms > 0 {
            tokio::time::sleep(Duration::from_millis(cfg.action_settle_ms)).await;
        }

        let display = args
            .display
            .or_else(|| session.last_screen.as_ref().map(|s| s.display));
        let cfg = cfg.clone();
        let shot = blocking(move || capture::capture(display, &cfg)).await?;
        session.last_screen = Some(shot.info.clone());

        images.push(ImageAttachment {
            mime: "image/png".into(),
            data_base64: shot.png_base64,
            width: shot.info.image_width,
            height: shot.info.image_height,
        });
        details["screenshot"] = json!("attached");
        details["display"] = json!(shot.info.display);
        details["image_width"] = json!(shot.info.image_width);
        details["image_height"] = json!(shot.info.image_height);
        if shot.appears_black {
            details["warning"] = json!(
                "The captured screen appears black. Check Screen Recording permission in \
                 System Settings → Privacy & Security → Screen Recording."
            );
        }
    }

    Ok(ComputerOutput {
        text: details.to_string(),
        images,
        screen: session.last_screen.clone(),
    })
}

async fn resolve_geometry(
    session: &ComputerSession,
    display: Option<usize>,
    max_width: u32,
) -> Result<ScreenGeometryInfo, ComputerError> {
    if let Some(last) = &session.last_screen
        && (display.is_none() || display == Some(last.display))
    {
        return Ok(last.clone());
    }
    blocking(move || capture::geometry(display, max_width)).await
}

fn screenshot_json(info: &ScreenGeometryInfo) -> Value {
    json!({
        "ok": true,
        "action": "screenshot",
        "display": info.display,
        "display_count": info.display_count,
        "screen_x": info.screen_x,
        "screen_y": info.screen_y,
        "screen_width": info.screen_width,
        "screen_height": info.screen_height,
        "image_width": info.image_width,
        "image_height": info.image_height,
        "scale_factor": info.scale_factor,
        "note": "The screenshot is attached in the next user message. Coordinates for \
                 subsequent actions are pixels in this image, origin top-left.",
    })
}

fn button(args: &ComputerArgs) -> Button {
    match args.button.as_deref() {
        Some("right") => Button::Right,
        Some("middle") => Button::Middle,
        _ => Button::Left,
    }
}

async fn blocking<T, F>(f: F) -> Result<T, ComputerError>
where
    F: FnOnce() -> Result<T, ComputerError> + Send + 'static,
    T: Send + 'static,
{
    task::spawn_blocking(f)
        .await
        .map_err(|e| ComputerError::Internal(format!("blocking task failed: {e}")))?
}
