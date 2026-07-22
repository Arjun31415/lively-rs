use hyprland::shared::HyprData;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// Returns the (x, y) offset of a specified monitor connector (e.g. "HDMI-A-1").
/// Defaults to (0, 0) if no connector name is specified or found.
pub fn get_monitor_offset(monitor_name: Option<&str>) -> (i64, i64) {
    if let Some(name) = monitor_name {
        if let Ok(monitors) = hyprland::data::Monitors::get() {
            for mon in monitors {
                if mon.name == name {
                    return (mon.x as i64, mon.y as i64);
                }
            }
        }
    }
    (0, 0)
}

/// Spawns a background thread that polls Hyprland's cursor position,
/// subtracts the target monitor's offset, and sends relative (x, y) coordinates.
pub fn start_mouse_tracking(
    monitor_name: Option<String>,
    tx: Sender<(i64, i64)>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let offset = get_monitor_offset(monitor_name.as_deref());
        let mut last_pos = (i64::MIN, i64::MIN);

        loop {
            if let Ok(cursor_pos) = hyprland::data::CursorPosition::get() {
                // Calculate position relative to target monitor top-left corner
                let rel_x = cursor_pos.x - offset.0;
                let rel_y = cursor_pos.y - offset.1;

                if last_pos != (rel_x, rel_y) {
                    last_pos = (rel_x, rel_y);
                    // Send updated position; break loop if channel disconnected
                    if tx.send((rel_x, rel_y)).is_err() {
                        break;
                    }
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
    })
}
