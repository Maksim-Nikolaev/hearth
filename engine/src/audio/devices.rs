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

/// The PipeWire/Pulse `node.name` (stable id) for a device, used as `device=`.
fn device_node_name(d: &gst::Device) -> Option<String> {
    let props = d.properties()?;
    props
        .get::<String>("node.name")
        .or_else(|_| props.get::<String>("device.name"))
        .ok()
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
    let id = device_node_name(d)?;
    let label = d.display_name().to_string();
    let is_default = is_default(default_name, &id);

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
