//! 自定义标题栏与 Harness 子 WebView 的布局。

use tauri::{LogicalPosition, LogicalSize, Manager, Rect, Window};

// 与前端 .titlebar 的 CSS 高度保持一致，使用逻辑像素以适配 DPI。
const TITLEBAR_HEIGHT: f64 = 36.0;
/// 信息面板（版本历史）与窗口右边缘/上边缘的留白。
const INFO_PANEL_MARGIN: f64 = 12.0;
/// 信息面板的期望尺寸；窗口更小时会按可用空间收缩。
/// 高度按当前内容估算:头部 + 四行版本信息 + 手动检查更新区块。
const INFO_PANEL_WIDTH: f64 = 340.0;
const INFO_PANEL_HEIGHT: f64 = 260.0;
/// 面板至少保留这么高，否则不再展示（窗口过矮时直接隐藏内容区）。
const INFO_PANEL_MIN_HEIGHT: f64 = 120.0;

pub(crate) fn harness_bounds(size: LogicalSize<f64>) -> Rect {
    let top = TITLEBAR_HEIGHT.min(size.height);
    Rect {
        position: LogicalPosition::new(0.0, top).into(),
        size: LogicalSize::new(size.width, (size.height - top).max(0.0)).into(),
    }
}

/// 信息面板的边界：贴右对齐、位于标题栏之下，并在窗口较小时收缩。
pub(crate) fn info_panel_bounds(size: LogicalSize<f64>) -> Rect {
    let available_height = (size.height - TITLEBAR_HEIGHT - INFO_PANEL_MARGIN).max(0.0);
    let width = INFO_PANEL_WIDTH.min((size.width - INFO_PANEL_MARGIN).max(0.0));
    let height = if available_height < INFO_PANEL_MIN_HEIGHT {
        available_height
    } else {
        INFO_PANEL_HEIGHT.min(available_height)
    };
    Rect {
        position: LogicalPosition::new(
            (size.width - width - INFO_PANEL_MARGIN).max(0.0),
            TITLEBAR_HEIGHT + INFO_PANEL_MARGIN,
        )
        .into(),
        size: LogicalSize::new(width, height).into(),
    }
}

/// 按当前窗口尺寸重新布局已存在的子 WebView。
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
    if let Some(info) = window.app_handle().get_webview("info") {
        info.set_bounds(info_panel_bounds(size))?;
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

    #[test]
    fn info_panel_stays_below_titlebar_and_inside_the_window() {
        for size in [
            LogicalSize::new(1280.0, 820.0),
            LogicalSize::new(800.0, 600.0),
            LogicalSize::new(420.0, 320.0),
        ] {
            let bounds = info_panel_bounds(size);
            let position = bounds.position.to_logical::<f64>(1.0);
            let content = bounds.size.to_logical::<f64>(1.0);
            assert!(position.y >= TITLEBAR_HEIGHT, "overlaps title bar at {size:?}");
            assert!(position.x >= 0.0, "escapes left edge at {size:?}");
            assert!(
                position.x + content.width <= size.width,
                "escapes right edge at {size:?}"
            );
            assert!(
                position.y + content.height <= size.height,
                "escapes bottom edge at {size:?}"
            );
            assert!(content.width > 0.0 && content.height > 0.0, "collapsed at {size:?}");
        }
    }

    #[test]
    fn info_panel_shrinks_with_a_tiny_window() {
        let bounds = info_panel_bounds(LogicalSize::new(400.0, 60.0));
        let content = bounds.size.to_logical::<f64>(1.0);
        // 60px window - 36px title bar - 12px margin leaves 12px, and a
        // negative height would be rejected by the native layer.
        assert_eq!(content.height, 12.0);
        assert!(content.width > 0.0);
    }
}
