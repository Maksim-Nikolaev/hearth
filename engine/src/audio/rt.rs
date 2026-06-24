/// Parse the soft "Max realtime priority" limit out of `/proc/self/limits`.
pub(crate) fn parse_rtprio_limit(proc_limits: &str) -> Option<u64> {
    let line = proc_limits
        .lines()
        .find(|l| l.starts_with("Max realtime priority"))?;

    // Columns after the label: "<soft> <hard> [units]".
    line.split_whitespace().nth(3).and_then(|s| s.parse().ok())
}

/// Whether the audio path can obtain realtime scheduling. On Linux this means
/// the user is permitted a non-zero RT priority (PAM limits / rtkit); PipeWire
/// then runs RT. Windows/macOS assume MMCSS/equivalent for now.
#[cfg(target_os = "linux")]
pub fn realtime_available() -> bool {
    match std::fs::read_to_string("/proc/self/limits") {
        Ok(s) => parse_rtprio_limit(&s).map(|n| n > 0).unwrap_or(false),
        Err(_) => false,
    }
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
