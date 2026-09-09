//! Display refresh rate detection.
//!
//! Queries the environment once at startup (no polling, no background timers) to
//! probe the display refresh rate for UI pacing (`ui.ups = "auto"`).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pacode_types::serde_json::{self, Value};

const CMD_TIMEOUT: Duration = Duration::from_millis(200);

/// Probes display refresh rate once, trying multiple compositor/X11 interfaces in order:
/// 1. `hyprctl monitors -j` (Hyprland)
/// 2. `wlr-randr --json` (wlroots Wayland)
/// 3. `xrandr --query` (X11 / XWayland)
/// 4. `/sys/class/drm/*/modes` (skipped: no refresh info available)
///
/// Returns the refresh rate rounded to the nearest integer and clamped to `1..=240`.
pub fn detect_refresh_hz() -> Option<u16> {
    // (a) hyprctl monitors -j
    if let Some(out) = run_cmd("hyprctl", &["monitors", "-j"])
        && let Some(hz) = parse_hyprctl_output(&out)
    {
        return Some(hz);
    }

    // (b) wlr-randr --json
    if let Some(out) = run_cmd("wlr-randr", &["--json"])
        && let Some(hz) = parse_wlr_randr_output(&out)
    {
        return Some(hz);
    }

    // (c) xrandr --query
    if let Some(out) = run_cmd("xrandr", &["--query"])
        && let Some(hz) = parse_xrandr_output(&out)
    {
        return Some(hz);
    }

    // (d) /sys/class/drm/*/modes has no refresh rate information, skip.
    None
}

/// Run an external command with a short timeout guard.
/// If the binary is missing or times out, returns `None`.
fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let reader_thread = std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        output
    });

    let start = Instant::now();
    let exited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => {
                if start.elapsed() >= CMD_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };

    if exited {
        reader_thread.join().ok()
    } else {
        None
    }
}

/// Round a refresh rate float to the nearest integer, clamped to `1..=240`.
pub fn sanitize_hz(hz: f64) -> Option<u16> {
    if hz.is_nan() || hz.is_infinite() || hz <= 0.0 {
        return None;
    }
    let rounded = hz.round();
    if rounded < 1.0 {
        Some(1)
    } else if rounded > 240.0 {
        Some(240)
    } else {
        Some(rounded as u16)
    }
}

/// Parse `hyprctl monitors -j` output:
/// takes the highest `refreshRate` of a focused monitor if any, otherwise of any monitor.
pub fn parse_hyprctl_output(json_text: &str) -> Option<u16> {
    let parsed: Value = serde_json::from_str(json_text).ok()?;
    let monitors = match &parsed {
        Value::Array(arr) => arr.as_slice(),
        Value::Object(_) => std::slice::from_ref(&parsed),
        _ => return None,
    };

    let mut focused_rates = Vec::new();
    let mut all_rates = Vec::new();

    for mon in monitors {
        let is_focused = mon
            .get("focused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let hz = mon
            .get("refreshRate")
            .or_else(|| mon.get("refresh_rate"))
            .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)));

        if let Some(rate) = hz {
            all_rates.push(rate);
            if is_focused {
                focused_rates.push(rate);
            }
        }
    }

    let chosen = if !focused_rates.is_empty() {
        focused_rates.into_iter().fold(0.0f64, f64::max)
    } else if !all_rates.is_empty() {
        all_rates.into_iter().fold(0.0f64, f64::max)
    } else {
        return None;
    };

    sanitize_hz(chosen)
}

/// Parse `wlr-randr --json` output:
/// inspects enabled outputs for `current_mode.refresh` or mode marked `current`.
pub fn parse_wlr_randr_output(json_text: &str) -> Option<u16> {
    let parsed: Value = serde_json::from_str(json_text).ok()?;
    let heads = match &parsed {
        Value::Array(arr) => arr.as_slice(),
        Value::Object(_) => std::slice::from_ref(&parsed),
        _ => return None,
    };

    let mut current_rates = Vec::new();
    let mut all_rates = Vec::new();

    for head in heads {
        let is_enabled = head
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let current_mode_rate = head.get("current_mode").and_then(extract_json_hz);
        let mut head_current = current_mode_rate;

        if let Some(modes) = head.get("modes").and_then(|m| m.as_array()) {
            for mode in modes {
                if let Some(hz) = extract_json_hz(mode) {
                    all_rates.push(hz);
                    let is_current = mode
                        .get("current")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if is_current && head_current.is_none() {
                        head_current = Some(hz);
                    }
                }
            }
        }

        if let Some(rate) = head_current {
            if is_enabled {
                current_rates.push(rate);
            } else {
                all_rates.push(rate);
            }
        }
    }

    let chosen = if !current_rates.is_empty() {
        current_rates.into_iter().fold(0.0f64, f64::max)
    } else if !all_rates.is_empty() {
        all_rates.into_iter().fold(0.0f64, f64::max)
    } else {
        return None;
    };

    sanitize_hz(chosen)
}

fn extract_json_hz(obj: &Value) -> Option<f64> {
    obj.get("refresh")
        .or_else(|| obj.get("refreshRate"))
        .or_else(|| obj.get("refresh_rate"))
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
}

/// Parse `xrandr --query` output:
/// finds lines with `*` indicating the active mode, extracting the refresh rate.
pub fn parse_xrandr_output(text: &str) -> Option<u16> {
    let mut rates = Vec::new();

    for line in text.lines() {
        if !line.contains('*') {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (i, &token) in tokens.iter().enumerate() {
            if let Some(idx) = token.find('*') {
                let prefix = &token[..idx];
                if let Ok(val) = prefix.parse::<f64>() {
                    rates.push(val);
                } else if i > 0
                    && let Ok(val) = tokens[i - 1].parse::<f64>()
                {
                    rates.push(val);
                }
            }
        }
    }

    if rates.is_empty() {
        return None;
    }

    let highest = rates.into_iter().fold(0.0f64, f64::max);
    sanitize_hz(highest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_hz() {
        assert_eq!(sanitize_hz(59.94), Some(60));
        assert_eq!(sanitize_hz(60.0), Some(60));
        assert_eq!(sanitize_hz(143.98), Some(144));
        assert_eq!(sanitize_hz(360.0), Some(240));
        assert_eq!(sanitize_hz(0.4), Some(1));
        assert_eq!(sanitize_hz(0.0), None);
        assert_eq!(sanitize_hz(-10.0), None);
        assert_eq!(sanitize_hz(f64::NAN), None);
        assert_eq!(sanitize_hz(f64::INFINITY), None);
    }

    #[test]
    fn test_parse_hyprctl() {
        let sample_focused = r#"[
            {
                "id": 0,
                "name": "DP-1",
                "refreshRate": 143.98,
                "focused": true
            },
            {
                "id": 1,
                "name": "HDMI-A-1",
                "refreshRate": 60.0,
                "focused": false
            }
        ]"#;
        assert_eq!(parse_hyprctl_output(sample_focused), Some(144));

        let sample_unfocused = r#"[
            {
                "id": 0,
                "name": "DP-1",
                "refreshRate": 165.0,
                "focused": false
            },
            {
                "id": 1,
                "name": "HDMI-A-1",
                "refreshRate": 60.0,
                "focused": false
            }
        ]"#;
        assert_eq!(parse_hyprctl_output(sample_unfocused), Some(165));

        assert_eq!(parse_hyprctl_output("invalid json"), None);
        assert_eq!(parse_hyprctl_output("[]"), None);
    }

    #[test]
    fn test_parse_wlr_randr() {
        let sample = r#"[
            {
                "name": "eDP-1",
                "enabled": true,
                "current_mode": {
                    "width": 1920,
                    "height": 1080,
                    "refresh": 60.001999
                }
            },
            {
                "name": "DP-2",
                "enabled": true,
                "current_mode": {
                    "width": 2560,
                    "height": 1440,
                    "refresh": 144.0
                }
            }
        ]"#;
        assert_eq!(parse_wlr_randr_output(sample), Some(144));

        let modes_sample = r#"[
            {
                "name": "eDP-1",
                "enabled": true,
                "modes": [
                    { "width": 1920, "height": 1080, "refresh": 59.94, "current": true },
                    { "width": 1920, "height": 1080, "refresh": 48.0, "current": false }
                ]
            }
        ]"#;
        assert_eq!(parse_wlr_randr_output(modes_sample), Some(60));

        assert_eq!(parse_wlr_randr_output("invalid"), None);
        assert_eq!(parse_wlr_randr_output("[]"), None);
    }

    #[test]
    fn test_parse_xrandr() {
        let sample = r#"
Screen 0: minimum 320 x 200, current 1920 x 1080, maximum 16384 x 16384
eDP-1 connected primary 1920x1080+0+0 (normal left inverted right x axis y axis) 344mm x 194mm
   1920x1080     60.00*+  48.00  
   1680x1050     60.00  
DP-1 disconnected
"#;
        assert_eq!(parse_xrandr_output(sample), Some(60));

        let sample_multi = r#"
eDP-1 connected 1920x1080+0+0
   1920x1080     60.00*+
HDMI-1 connected 1920x1080+1920+0
   1920x1080     60.00 +  74.97*   50.00
"#;
        assert_eq!(parse_xrandr_output(sample_multi), Some(75));

        let sample_separate_star = r#"
eDP-1 connected 1920x1080+0+0
   1920x1080     144.00 *+
"#;
        assert_eq!(parse_xrandr_output(sample_separate_star), Some(144));

        assert_eq!(parse_xrandr_output("no star here"), None);
        assert_eq!(parse_xrandr_output(""), None);
    }
}
