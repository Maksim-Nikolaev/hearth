use crate::audio::devices::device_to_info;
use crate::audio::profile::OutputKind;
use gstreamer as gst;
use gstreamer::prelude::*;

/// Classify a string against known form-factor / name hints.
pub(crate) fn kind_from(form_factor: Option<&str>, label: &str) -> OutputKind {
    let ff = form_factor.unwrap_or("").to_ascii_lowercase();
    if ff.contains("head") || ff.contains("hands-free") || ff.contains("earbud") {
        return OutputKind::Headphones;
    }
    if ff.contains("speaker") || ff == "internal" || ff.contains("hifi") || ff == "tv" {
        return OutputKind::Speakers;
    }

    let l = label.to_ascii_lowercase();
    if l.contains("headphone") || l.contains("headset") || l.contains("earbud") {
        return OutputKind::Headphones;
    }
    if l.contains("speaker") {
        return OutputKind::Speakers;
    }
    OutputKind::Unknown
}

/// Classify the active output device. Linux reads the sink's PipeWire/Pulse
/// `device.form_factor` and display name; Windows is deferred (always `Unknown`).
/// `output_id` is the saved device id (`None` = system default).
#[cfg(target_os = "windows")]
pub fn classify_output(_output_id: Option<&str>) -> OutputKind {
    // TODO: WASAPI PKEY_AudioEndpoint_FormFactor.
    OutputKind::Unknown
}

#[cfg(not(target_os = "windows"))]
pub fn classify_output(output_id: Option<&str>) -> OutputKind {
    let _ = gst::init();
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::new_empty_simple("audio/x-raw");
    let _ = monitor.add_filter(Some("Audio/Sink"), Some(&caps));
    if monitor.start().is_err() {
        return OutputKind::Unknown;
    }
    let devices = monitor.devices();
    monitor.stop();

    for d in devices.iter() {
        let Some(info) = device_to_info(d, None) else { continue };
        let matches = match output_id {
            Some(id) => info.id == id,
            None => info.is_default,
        };
        if !matches {
            continue;
        }
        let form_factor = d
            .properties()
            .and_then(|p| p.get::<String>("device.form_factor").ok());
        return kind_from(form_factor.as_deref(), &d.display_name());
    }
    OutputKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_factor_wins() {
        assert_eq!(kind_from(Some("headphone"), "whatever"), OutputKind::Headphones);
        assert_eq!(kind_from(Some("headset"), "x"), OutputKind::Headphones);
        assert_eq!(kind_from(Some("speaker"), "x"), OutputKind::Speakers);
        assert_eq!(kind_from(Some("internal"), "x"), OutputKind::Speakers);
    }

    #[test]
    fn label_fallback_when_no_form_factor() {
        assert_eq!(kind_from(None, "Logitech PRO X Headphones"), OutputKind::Headphones);
        assert_eq!(kind_from(None, "Built-in Speaker"), OutputKind::Speakers);
        assert_eq!(kind_from(None, "Generic USB Audio"), OutputKind::Unknown);
    }

    #[test]
    fn unknown_form_factor_falls_through_to_label() {
        assert_eq!(kind_from(Some("car"), "USB Headset"), OutputKind::Headphones);
        assert_eq!(kind_from(Some("car"), "Mystery Box"), OutputKind::Unknown);
    }
}
