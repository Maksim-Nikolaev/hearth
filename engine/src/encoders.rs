use gstreamer as gst;

const CANDIDATES: &[(&str, &str)] = &[
    ("amfh265enc", "AMD AMF HEVC"),
    ("vah265enc", "VA-API HEVC (modern)"),
    ("vaapih265enc", "VA-API HEVC (legacy)"),
    ("nvh265enc", "NVIDIA NVENC HEVC"),
    ("qsvh265enc", "Intel QuickSync HEVC"),
    ("vtenc_h265", "Apple VideoToolbox HEVC"),
    ("x265enc", "software HEVC (fallback)"),
];

/// First available encoder factory name, plus the full availability list.
pub fn detect() -> (Option<&'static str>, Vec<(&'static str, &'static str, bool)>) {
    let mut list = Vec::new();
    let mut chosen = None;

    for (factory, label) in CANDIDATES {
        let available = gst::ElementFactory::find(factory).is_some();

        if available && chosen.is_none() {
            chosen = Some(*factory);
        }

        list.push((*factory, *label, available));
    }

    (chosen, list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_is_consistent() {
        gst::init().unwrap();

        let (chosen, list) = detect();

        assert_eq!(list.len(), 7);
        assert_eq!(chosen.is_some(), list.iter().any(|(_, _, ok)| *ok));
    }
}
