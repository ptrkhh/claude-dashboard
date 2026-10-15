use super::cmd::Runner;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Battery {
    pub pct: u8,
    pub plugged: bool,
}

fn reading(pct: u8, plugged: bool) -> Option<Battery> {
    (pct <= 100).then_some(Battery { pct, plugged })
}

/// Read the server's battery, never the browser/client device's battery.
pub async fn battery_status(runner: &Runner) -> Option<Battery> {
    if cfg!(target_os = "macos") {
        return parse_pmset(
            &runner
                .run("/usr/bin/pmset", &["-g", "batt"], "battery pmset")
                .await,
        );
    }
    if cfg!(target_os = "linux") {
        let wsl = std::env::var_os("WSL_INTEROP").is_some()
            || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("microsoft");
        if !wsl {
            return linux_battery(Path::new("/sys/class/power_supply"));
        }
        // Windows PowerShell is installed with Windows; no extra package needed.
        let installed = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
        let program = if Path::new(installed).is_file() {
            installed
        } else {
            "powershell.exe"
        };
        let out = runner
            .run(
                program,
                &[
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_BATTERY,
                ],
                "battery Windows",
            )
            .await;
        return parse_windows(&out);
    }
    None
}

// PowerStatus wraps GetSystemPowerStatus. Charging and AC connection differ:
// a full battery can still be plugged in. Reject unknown/no-battery flags.
const WINDOWS_BATTERY: &str = r#"Add-Type -AssemblyName System.Windows.Forms
$s = [System.Windows.Forms.SystemInformation]::PowerStatus
$flags = [int]$s.BatteryChargeStatus
$ac = [int]$s.PowerLineStatus
$pct = [double]$s.BatteryLifePercent
if ($flags -ne 255 -and ($flags -band 128) -eq 0 -and $ac -in 0,1 -and $pct -ge 0 -and $pct -le 1) {
    '{0} {1}' -f [int][Math]::Round($pct * 100), $ac
}"#;

fn parse_windows(out: &str) -> Option<Battery> {
    let mut fields = out.split_whitespace();
    let pct = fields.next()?.parse().ok()?;
    let plugged = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    if fields.next().is_some() {
        return None;
    }
    reading(pct, plugged)
}

fn parse_pmset(out: &str) -> Option<Battery> {
    let source = out.lines().next()?;
    let plugged = if source.contains("'AC Power'") {
        true
    } else if source.contains("'Battery Power'") {
        false
    } else {
        return None;
    };
    let line = out
        .lines()
        .find(|line| line.contains("InternalBattery") && !line.contains("present: false"))?;
    let pct = line
        .split_once('%')?
        .0
        .split_whitespace()
        .last()?
        .parse()
        .ok()?;
    reading(pct, plugged)
}

fn linux_battery(root: &Path) -> Option<Battery> {
    let supplies: Vec<_> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    let text = |path: &Path, field: &str| {
        std::fs::read_to_string(path.join(field))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    let online: Vec<_> = supplies
        .iter()
        .filter(|p| text(p, "type").as_deref().is_some_and(|t| t != "Battery"))
        .filter(|p| text(p, "scope").as_deref() != Some("Device"))
        .filter_map(|p| match text(p, "online").as_deref() {
            Some("0") => Some(false),
            Some("1") => Some(true),
            _ => None,
        })
        .collect();
    // ponytail: first readable system battery; aggregate if multi-battery hosts need it.
    supplies.iter().find_map(|path| {
        if text(path, "type").as_deref() != Some("Battery")
            || text(path, "present").as_deref() == Some("0")
            || text(path, "scope").as_deref() == Some("Device")
        {
            return None;
        }
        let pct = text(path, "capacity")?.parse().ok()?;
        let plugged = if online.contains(&true) {
            true
        } else {
            match text(path, "status").as_deref() {
                Some("Charging") => true,
                Some("Discharging") => false,
                _ if !online.is_empty() => false,
                _ => return None,
            }
        };
        reading(pct, plugged)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_readings_require_percentage_and_known_power_source() {
        assert_eq!(parse_windows("0 0\r\n"), reading(0, false));
        assert_eq!(parse_windows("100 1\r\n"), reading(100, true));
        for out in ["", "255 1", "50 255", "50", "50 1 error", "-1 0"] {
            assert_eq!(parse_windows(out), None, "{out}");
        }
        let battery = " -InternalBattery-0 (id=123)\t75%; charged; 0:00 remaining present: true";
        assert_eq!(
            parse_pmset(&format!("Now drawing from 'AC Power'\n{battery}")),
            reading(75, true)
        );
        assert_eq!(
            parse_pmset(&format!("Now drawing from 'Battery Power'\n{battery}")),
            reading(75, false)
        );
        assert_eq!(parse_pmset("Now drawing from 'AC Power'"), None);
        assert_eq!(parse_pmset(&format!("Unknown source\n{battery}")), None);
        assert_eq!(
            parse_pmset(&format!(
                "Now drawing from 'AC Power'\n{}",
                battery.replace("75%", "255%")
            )),
            None
        );
    }

    #[test]
    fn linux_requires_a_system_battery_and_known_connection() {
        let root = std::env::temp_dir().join(format!("cdash-battery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bat = root.join("BAT0");
        let ac = root.join("AC");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::create_dir_all(&ac).unwrap();
        let write =
            |dir: &Path, field: &str, value: &str| std::fs::write(dir.join(field), value).unwrap();
        write(&ac, "type", "Mains\n");
        write(&ac, "online", "1\n");
        assert_eq!(linux_battery(&root), None);
        write(&bat, "type", "Battery\n");
        write(&bat, "capacity", "100\n");
        write(&bat, "status", "Full\n");
        assert_eq!(linux_battery(&root), reading(100, true));
        write(&ac, "online", "0\n");
        write(&bat, "capacity", "0\n");
        write(&bat, "status", "Discharging\n");
        assert_eq!(linux_battery(&root), reading(0, false));
        std::fs::remove_file(ac.join("online")).unwrap();
        assert_eq!(linux_battery(&root), reading(0, false));
        write(&bat, "status", "Charging\n");
        assert_eq!(linux_battery(&root), reading(0, true));
        write(&bat, "status", "Unknown\n");
        assert_eq!(linux_battery(&root), None);
        write(&ac, "online", "1\n");
        write(&bat, "capacity", "101\n");
        assert_eq!(linux_battery(&root), None);
        write(&bat, "capacity", "50\n");
        write(&bat, "present", "0\n");
        assert_eq!(linux_battery(&root), None);
        write(&bat, "present", "1\n");
        write(&bat, "scope", "Device\n");
        assert_eq!(linux_battery(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
