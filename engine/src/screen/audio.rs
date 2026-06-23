#[cfg(not(target_os = "windows"))]
use std::process::Command;

/// Which audio source to capture alongside the screenshare, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ShareAudio {
    /// No audio track is sent with the screenshare.
    #[default]
    None,
    /// Capture the default PulseAudio/PipeWire sink monitor (system output).
    System,
    /// Capture a specific PipeWire application node by its node name.
    App { node: String },
}

/// A PipeWire audio node suitable for per-app capture.
#[derive(Debug, Clone)]
pub struct AudioNode {
    /// Node name (pass to `pipewiresrc target-object=`).
    pub node: String,
    /// Human-readable label (application name or description).
    pub label: String,
}

/// Raw properties of a PipeWire node, as parsed from `pw-dump`. (Linux only.)
#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone)]
pub struct NodeProps {
    pub pid: u32,
    pub media_class: String,
    pub virtual_node: bool,
}

/// Filtering options for [`keep_node`]. (Linux only.)
#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone)]
pub struct NodeFilter {
    /// Drop nodes whose `media.class` contains `"Input"`.
    pub ignore_input: bool,
    /// Drop virtual nodes (loopbacks, network sinks, etc.).
    pub ignore_virtual: bool,
    /// Drop device-level nodes (not application streams).
    pub ignore_devices: bool,
    /// Reserved: not yet enforced by [`keep_node`]. Determining which nodes are
    /// speaker-routed requires routing graph info not currently captured from
    /// `pw-dump`.
    pub only_speakers: bool,
}

#[cfg(not(target_os = "windows"))]
impl Default for NodeFilter {
    fn default() -> Self {
        Self {
            ignore_input: true,
            ignore_virtual: false,
            ignore_devices: true,
            only_speakers: true,
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl NodeFilter {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Returns `true` when the node should be included in the listing.
///
/// Always excludes the process with `own_pid` to prevent a feedback loop when
/// the sharer's own renderer appears as a capturable node. (Linux only.)
#[cfg(not(target_os = "windows"))]
pub(crate) fn keep_node(props: &NodeProps, own_pid: u32, filt: &NodeFilter) -> bool {
    if props.pid == own_pid {
        return false;
    }

    if filt.ignore_input && props.media_class.contains("Input") {
        return false;
    }

    if filt.ignore_virtual && props.virtual_node {
        return false;
    }

    true
}

/// Returns `true` when PipeWire is available at runtime (the `pipewiresrc`
/// GStreamer plugin factory can be found). Required only for *per-application*
/// audio capture.
pub fn has_pipewire() -> bool {
    gstreamer::ElementFactory::find("pipewiresrc").is_some()
}

/// Returns `true` when whole-system output capture is available — the platform
/// loopback/monitor source element exists. Unlike per-app capture this does not
/// need PipeWire (Windows uses `wasapi2src loopback`, Linux the `pulsesrc`
/// sink monitor).
pub fn has_system_audio() -> bool {
    let factory = if cfg!(target_os = "windows") {
        "wasapi2src"
    } else {
        "pulsesrc"
    };
    gstreamer::ElementFactory::find(factory).is_some()
}

/// List per-application audio output sources, excluding our own process.
///
/// Linux: PipeWire output nodes (via `pw-dump`). Windows: processes with an
/// active WASAPI audio session, captured by pid through `wasapi2src` process
/// loopback. Returns an empty `Vec` when unavailable.
#[cfg(target_os = "windows")]
pub fn list_app_nodes() -> Vec<AudioNode> {
    super::audio_win::list_audio_sessions()
}

#[cfg(not(target_os = "windows"))]
pub fn list_app_nodes() -> Vec<AudioNode> {
    if !has_pipewire() {
        return Vec::new();
    }

    list_app_nodes_inner().unwrap_or_default()
}

#[cfg(not(target_os = "windows"))]
fn list_app_nodes_inner() -> Option<Vec<AudioNode>> {
    let own_pid = std::process::id();
    let output = Command::new("pw-dump").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let filt = NodeFilter::default();

    Some(parse_nodes(&json, own_pid, &filt))
}

/// Parse a `pw-dump` JSON value (top-level array) into a list of audio nodes.
///
/// Non-node entries (links, factories, devices, etc.) are skipped rather than
/// aborting the parse, so the returned `Vec` reflects all valid nodes found.
#[cfg(not(target_os = "windows"))]
pub(crate) fn parse_nodes(json: &serde_json::Value, own_pid: u32, filt: &NodeFilter) -> Vec<AudioNode> {
    let Some(arr) = json.as_array() else {
        return Vec::new();
    };

    let mut nodes = Vec::new();

    for item in arr {
        let Some(obj) = item.as_object() else { continue; };

        let type_str = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if type_str != "PipeWire:Interface:Node" {
            continue;
        }

        let Some(info) = obj.get("info").and_then(|v| v.as_object()) else { continue; };
        let Some(props) = info.get("props").and_then(|v| v.as_object()) else { continue; };

        let media_class = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Only application stream nodes (not hardware device nodes).
        if !media_class.starts_with("Stream/") {
            continue;
        }

        let pid = props
            .get("application.process.id")
            .or_else(|| props.get("pipewire.sec.pid"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let virtual_node = props
            .get("node.virtual")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let node_props = NodeProps { pid, media_class, virtual_node };

        if !keep_node(&node_props, own_pid, &filt) {
            continue;
        }

        let node_name = props
            .get("node.name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if node_name.is_empty() {
            continue;
        }

        let app_name = props
            .get("application.name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Prefer application.name as the base. When absent fall back to
        // node.description then node.name so something always shows.
        let base = if !app_name.is_empty() {
            app_name
        } else {
            props
                .get("node.description")
                .or_else(|| props.get("node.name"))
                .and_then(|v| v.as_str())
                .unwrap_or(&node_name)
        };

        // Append a detail suffix when media.name carries stream-level context
        // (e.g. a tab title) that differs from the base, so two streams from
        // the same app render as distinct labels.
        let media_name = props
            .get("media.name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let label = if !media_name.is_empty() && media_name != base {
            format!("{base} \u{2013} {media_name}")
        } else {
            base.to_string()
        };

        nodes.push(AudioNode { node: node_name, label });
    }

    nodes
}

/// Build the GStreamer source chain string for the given audio choice.
///
/// Returns `None` for [`ShareAudio::None`]. The returned string is suitable
/// as the source portion of a `gst_parse_launch` description, producing
/// stereo Opus at 48 kHz, ready to feed into `rtpopuspay`.
pub fn screen_audio_chain(a: &ShareAudio) -> Option<String> {
    match a {
        ShareAudio::None => None,
        ShareAudio::System => Some(with_opus_tail(&system_audio_src())),
        ShareAudio::App { node } => Some(with_opus_tail(&app_audio_src(node))),
    }
}

/// Append the shared convert/resample/encode tail to a source element string,
/// producing stereo 48 kHz Opus ready for `rtpopuspay`.
///
/// The `queue leaky=downstream` right after the source decouples the live
/// capture thread from the encoder/webrtc. Without it, while the screen
/// `webrtcbin` is still negotiating, downstream stalls and back-pressures the
/// live source — on Windows that overruns the WASAPI ring buffer and the device
/// gets invalidated (`AUDCLNT_E_DEVICE_INVALIDATED`), which kills the shared
/// screen pipeline (black video). Leaking drops old audio instead.
fn with_opus_tail(src: &str) -> String {
    format!(
        "{src} \
         ! queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=200000000 \
         ! audioconvert \
         ! audioresample \
         ! audio/x-raw,rate=48000,channels=2 \
         ! opusenc audio-type=generic"
    )
}

/// System-output capture source. Linux/macOS tap the default PulseAudio sink
/// monitor; Windows uses a WASAPI render-endpoint loopback.
#[cfg(target_os = "windows")]
fn system_audio_src() -> String {
    // Exclude our own process from the system mix so the shared audio doesn't
    // include the peers' voice the app is already playing back — otherwise they
    // hear themselves echoed. (Discord-style "system audio minus the call".)
    format!(
        "wasapi2src loopback=true low-latency=true \
         loopback-mode=exclude-process-tree loopback-target-pid={}",
        std::process::id()
    )
}
#[cfg(not(target_os = "windows"))]
fn system_audio_src() -> String {
    "pulsesrc device=@DEFAULT_SINK@.monitor".to_string()
}

/// Per-application capture source. PipeWire targets a node by name; Windows uses
/// WASAPI process loopback, capturing the target pid and its child processes.
/// A non-numeric node on Windows falls back to whole-system loopback.
#[cfg(target_os = "windows")]
fn app_audio_src(node: &str) -> String {
    match node.parse::<u32>() {
        Ok(pid) => format!(
            "wasapi2src loopback=true low-latency=true loopback-mode=include-process-tree loopback-target-pid={pid}"
        ),
        Err(_) => system_audio_src(),
    }
}
#[cfg(not(target_os = "windows"))]
fn app_audio_src(node: &str) -> String {
    format!("pipewiresrc target-object={node}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "windows"))]
    fn props(pid: u32, media_class: &str, virt: bool) -> NodeProps {
        NodeProps { pid, media_class: media_class.into(), virtual_node: virt }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn excludes_own_process() {
        let f = NodeFilter::default();

        assert!(!keep_node(&props(42, "Stream/Output/Audio", false), 42, &f));
        assert!(keep_node(&props(99, "Stream/Output/Audio", false), 42, &f));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn excludes_inputs_and_virtual_when_requested() {
        let f = NodeFilter { ignore_input: true, ignore_virtual: true, ..Default::default() };

        assert!(!keep_node(&props(1, "Stream/Input/Audio", false), 0, &f));
        assert!(!keep_node(&props(1, "Stream/Output/Audio", true), 0, &f));
        assert!(keep_node(&props(1, "Stream/Output/Audio", false), 0, &f));
    }

    #[test]
    fn system_chain_uses_platform_source() {
        assert!(screen_audio_chain(&ShareAudio::None).is_none());

        let system = screen_audio_chain(&ShareAudio::System).unwrap();
        assert!(system.contains("opusenc"));
        #[cfg(target_os = "windows")]
        assert!(system.contains("wasapi2src") && system.contains("loopback=true"));
        #[cfg(not(target_os = "windows"))]
        assert!(system.contains(".monitor"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn app_chain_targets_the_node() {
        let app = screen_audio_chain(&ShareAudio::App { node: "Firefox".into() }).unwrap();
        assert!(app.contains("target-object=Firefox"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn app_chain_targets_pid_on_windows() {
        let app = screen_audio_chain(&ShareAudio::App { node: "1234".into() }).unwrap();
        assert!(app.contains("loopback-target-pid=1234"));
        assert!(app.contains("loopback-mode=include-process-tree"));
        assert!(app.contains("opusenc"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn parse_nodes_returns_app_output_stream_with_friendly_label() {
        // application.name is the base; a distinct media.name is appended so
        // streams from the same app can be told apart in the UI.
        let json: serde_json::Value = serde_json::json!([
            {
                "type": "PipeWire:Interface:Node",
                "id": 77,
                "info": {
                    "props": {
                        "media.class": "Stream/Output/Audio",
                        "node.name": "chromium",
                        "application.name": "Chromium",
                        "media.name": "AudioStream",
                        "application.process.id": 1234
                    }
                }
            }
        ]);

        let filt = NodeFilter::default();
        // own_pid differs so the node is not self-excluded.
        let nodes = parse_nodes(&json, 9999, &filt);

        assert_eq!(nodes.len(), 1, "one app output stream must be returned");
        assert_eq!(nodes[0].node, "chromium", "node field carries the node.name");
        assert_eq!(nodes[0].label, "Chromium \u{2013} AudioStream", "label appends distinct media.name");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn parse_nodes_labels_same_app_streams_distinctly() {
        // Two Firefox streams with the same application.name but different
        // media.name values (e.g. two tabs playing audio simultaneously).
        // Labels must be distinct; node identifiers must be preserved for capture.
        let json: serde_json::Value = serde_json::json!([
            {
                "type": "PipeWire:Interface:Node",
                "id": 101,
                "info": {
                    "props": {
                        "media.class": "Stream/Output/Audio",
                        "node.name": "Firefox-101",
                        "application.name": "Firefox",
                        "media.name": "How We Built Smart Enemies – YouTube",
                        "application.process.id": 5001,
                        "object.serial": 101
                    }
                }
            },
            {
                "type": "PipeWire:Interface:Node",
                "id": 102,
                "info": {
                    "props": {
                        "media.class": "Stream/Output/Audio",
                        "node.name": "Firefox-102",
                        "application.name": "Firefox",
                        "media.name": "Rust in 100 Seconds – YouTube",
                        "application.process.id": 5001,
                        "object.serial": 102
                    }
                }
            }
        ]);

        let filt = NodeFilter::default();
        let nodes = parse_nodes(&json, 9999, &filt);

        assert_eq!(nodes.len(), 2, "both Firefox streams must be returned");

        // Node identifiers are preserved (used for target-object= in capture).
        assert_eq!(nodes[0].node, "Firefox-101");
        assert_eq!(nodes[1].node, "Firefox-102");

        // Labels must be distinct despite the same application.name.
        assert_ne!(nodes[0].label, nodes[1].label, "same-app streams need distinct labels");
        assert!(nodes[0].label.starts_with("Firefox \u{2013}"), "label includes app name");
        assert!(nodes[1].label.starts_with("Firefox \u{2013}"), "label includes app name");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn parse_nodes_skips_non_node_entries() {
        // A realistic pw-dump array mixing a valid audio stream node with a
        // non-node entry (a Link interface). The non-node must not cause the
        // whole parse to bail out early.
        let json: serde_json::Value = serde_json::json!([
            {
                "type": "PipeWire:Interface:Link",
                "id": 10,
                "info": {}
            },
            {
                "type": "PipeWire:Interface:Node",
                "id": 42,
                "info": {
                    "props": {
                        "media.class": "Stream/Output/Audio",
                        "node.name": "Firefox",
                        "application.name": "Firefox",
                        "application.process.id": 9999
                    }
                }
            }
        ]);

        let filt = NodeFilter::default();
        // Use a pid that doesn't match the node's pid so it isn't filtered out.
        let nodes = parse_nodes(&json, 1, &filt);

        assert!(!nodes.is_empty(), "non-node entry must not cause empty result");
        assert_eq!(nodes[0].node, "Firefox");
    }

    #[test]
    #[ignore] // live: prints processes with an active audio session
    fn lists_live_app_nodes() {
        let nodes = list_app_nodes();
        println!("{nodes:#?}");
    }
}
