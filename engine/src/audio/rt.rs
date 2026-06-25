/// Parse the soft "Max realtime priority" limit out of `/proc/self/limits`. Only
/// the Linux `realtime_available` reads it (and the tests); other targets don't.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_rtprio_limit(proc_limits: &str) -> Option<u64> {
    let line = proc_limits
        .lines()
        .find(|l| l.starts_with("Max realtime priority"))?;

    // Columns after the label: "<soft> <hard> [units]".
    line.split_whitespace().nth(3).and_then(|s| s.parse().ok())
}

/// True when RealtimeKit is installed. rtkit grants RT priority dynamically over
/// D-Bus, bypassing `RLIMIT_RTPRIO` — which is how PipeWire gets realtime on most
/// desktops even though the rlimit reads 0. Presence of its service file (or the
/// daemon binary) means RT is obtainable on request.
#[cfg(target_os = "linux")]
fn rtkit_present() -> bool {
    [
        "/usr/share/dbus-1/system-services/org.freedesktop.RealtimeKit1.service",
        "/usr/libexec/rtkit-daemon",
        "/usr/lib/rtkit/rtkit-daemon",
        "/usr/lib/rtkit-daemon",
    ]
    .iter()
    .any(|p| std::path::Path::new(p).exists())
}

/// Whether the audio path can obtain realtime scheduling. On Linux that is true
/// if the user has a non-zero RT priority limit (PAM limits) OR rtkit can grant
/// it on demand; PipeWire then runs RT. Windows/macOS assume MMCSS/equivalent.
#[cfg(target_os = "linux")]
pub fn realtime_available() -> bool {
    let rlimit_ok = std::fs::read_to_string("/proc/self/limits")
        .ok()
        .and_then(|s| parse_rtprio_limit(&s))
        .map(|n| n > 0)
        .unwrap_or(false);

    rlimit_ok || rtkit_present()
}

#[cfg(not(target_os = "linux"))]
pub fn realtime_available() -> bool {
    // TODO(Windows): confirm MMCSS "Pro Audio" registration.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Limit                     Soft Limit           Hard Limit           Units
Max cpu time              unlimited            unlimited            seconds
Max realtime priority     95                   95
Max realtime timeout      unlimited            unlimited            us";

    const SAMPLE_ZERO: &str = "Max realtime priority     0                    0";

    #[test]
    fn parses_soft_rtprio() {
        assert_eq!(parse_rtprio_limit(SAMPLE), Some(95));
        assert_eq!(parse_rtprio_limit(SAMPLE_ZERO), Some(0));
        assert_eq!(parse_rtprio_limit("nothing here"), None);
    }
}
