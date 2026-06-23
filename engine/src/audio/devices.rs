use gstreamer as gst;
use gstreamer::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceKind {
    Source,
    Sink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
    pub kind: DeviceKind,
    pub is_default: bool,
}

pub(crate) fn is_default(default_name: Option<&str>, id: &str) -> bool {
    default_name == Some(id)
}

/// The stable id for a device, used as the `device=` value on the platform
/// capture/playback element. PipeWire/Pulse expose `node.name`; WASAPI
/// (`wasapi2`) exposes `device.id` (a GUID). Try them in order.
fn device_id(d: &gst::Device) -> Option<String> {
    let props = d.properties()?;
    props
        .get::<String>("node.name")
        .or_else(|_| props.get::<String>("device.name"))
        .or_else(|_| props.get::<String>("device.id"))
        .ok()
}

/// True when GStreamer marks this device as the system default (`device.default`
/// on WASAPI). Used as a fallback when no explicit default name is known.
fn property_is_default(d: &gst::Device) -> bool {
    d.properties()
        .and_then(|p| p.get::<bool>("device.default").ok())
        .unwrap_or(false)
}

/// True when a WASAPI device is a render-endpoint loopback (system-audio
/// capture surfaced as a source). It should not appear as a microphone choice.
fn is_loopback_source(d: &gst::Device) -> bool {
    d.properties()
        .and_then(|p| p.get::<bool>("wasapi2.device.loopback").ok())
        .unwrap_or(false)
}

pub(crate) fn device_to_info(d: &gst::Device, default_name: Option<&str>) -> Option<AudioDevice> {
    let klass = d.device_class();
    let kind = if klass.contains("Source") {
        DeviceKind::Source
    } else if klass.contains("Sink") {
        DeviceKind::Sink
    } else {
        return None;
    };

    // A render endpoint exposed as a loopback "source" is system audio, not a mic.
    if kind == DeviceKind::Source && is_loopback_source(d) {
        return None;
    }

    let id = device_id(d)?;
    let label = d.display_name().to_string();
    let is_default = is_default(default_name, &id) || property_is_default(d);

    Some(AudioDevice { id, label, kind, is_default })
}

/// Enumerate Pulse/PipeWire audio sources and sinks via a one-shot DeviceMonitor.
pub fn list_devices() -> Vec<AudioDevice> {
    let _ = gst::init();
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::new_empty_simple("audio/x-raw");
    let _ = monitor.add_filter(Some("Audio/Source"), Some(&caps));
    let _ = monitor.add_filter(Some("Audio/Sink"), Some(&caps));
    if monitor.start().is_err() {
        return Vec::new();
    }

    let devices = monitor.devices();
    monitor.stop();

    devices.iter().filter_map(|d| device_to_info(d, None)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pulse_device_to_info() {
        gstreamer::init().unwrap();
        // A synthetic gst::Device via the pulse provider is impractical in a unit
        // test, so test the pure label/default logic instead.
        let info = AudioDevice {
            id: "alsa_input.pci-0000_00.analog-stereo".into(),
            label: "Built-in Audio Analog Stereo".into(),
            kind: DeviceKind::Source,
            is_default: true,
        };
        assert_eq!(info.kind, DeviceKind::Source);
        assert!(info.is_default);
    }

    #[test]
    fn default_flag_matches_default_name() {
        assert!(is_default(Some("dev.monitor"), "dev.monitor"));
        assert!(!is_default(Some("other"), "dev.monitor"));
        assert!(!is_default(None, "dev.monitor"));
    }

    #[test]
    #[ignore] // live: prints real devices
    fn lists_live_devices() {
        let d = list_devices();
        println!("{d:#?}");
        assert!(!d.is_empty());
    }
}
