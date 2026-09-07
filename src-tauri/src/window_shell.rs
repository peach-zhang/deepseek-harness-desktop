//! 自定义标题栏与 Harness 子 WebView 的布局。

use tauri::{LogicalPosition, LogicalSize, Manager, Rect, Window};

// 与前端 .titlebar 的 CSS 高度保持一致，使用逻辑像素以适配 DPI。
const TITLEBAR_HEIGHT: f64 = 36.0;

pub(crate) fn harness_bounds(size: LogicalSize<f64>) -> Rect {
    let top = TITLEBAR_HEIGHT.min(size.height);
    Rect {
        position: LogicalPosition::new(0.0, top).into(),
        size: LogicalSize::new(size.width, (size.height - top).max(0.0)).into(),
    }
}

pub(crate) fn resize_webviews(window: &Window) -> tauri::Result<()> {
    let size = window.inner_size()?.to_logical::<f64>(window.scale_factor()?);
    // 最小化时保留原布局，避免给原生 WebView 设置零尺寸。
    if size.width <= 0.0 || size.height <= TITLEBAR_HEIGHT {
        return Ok(());
    }
    let harness = window.app_handle().get_webview("harness");
    if let Some(bootstrap) = window.app_handle().get_webview("main") {
        let height = if harness.is_some() {
            TITLEBAR_HEIGHT
        } else {
            size.height
        };
        bootstrap.set_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(size.width, height).into(),
        })?;
    }
    if let Some(harness) = harness {
        harness.set_bounds(harness_bounds(size))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_stays_below_titlebar_at_each_scale_factor() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let size = tauri::PhysicalSize::new(1280, 820).to_logical::<f64>(scale);
            let bounds = harness_bounds(size);
            let position = bounds.position.to_logical::<f64>(scale);
            let content = bounds.size.to_logical::<f64>(scale);
            assert_eq!(position.y, TITLEBAR_HEIGHT);
            assert_eq!(position.x, 0.0);
            assert_eq!(content.width, size.width);
            assert_eq!(position.y + content.height, size.height);
        }
    }

    #[test]
    fn tiny_windows_never_produce_negative_content_sizes() {
        let bounds = harness_bounds(LogicalSize::new(0.0, 10.0));
        assert_eq!(bounds.position.to_logical::<f64>(1.0).y, 10.0);
        assert_eq!(bounds.size.to_logical::<f64>(1.0).height, 0.0);
    }
}
