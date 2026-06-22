use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use engine::screen::audio::{has_pipewire, list_app_nodes, ShareAudio};
use engine::screen::sources::{list_windows, ContentType, ShareConfig, ShareSource, ShareWindow};
use engine::screen::thumbnail::thumbnail;
use gtk::prelude::*;
use relm4::prelude::*;

/// The picker exposes the currently selected config so the root can start /
/// refresh a preview pipeline without sending the non-`Send` paintable through
/// a message.
#[derive(Debug, Clone)]
pub enum PickerOutput {
    /// Source, quality, or audio changed – root should refresh the preview.
    ConfigChanged(ShareConfig),
    /// User confirmed – start the real share.
    GoLive(ShareConfig),
    /// User dismissed without going live.
    Cancel,
}

/// Rows in the audio drop-down.
#[derive(Debug, Clone)]
enum AudioRow {
    None,
    System,
    App { node: String, label: String },
}

impl AudioRow {
    fn label(&self) -> String {
        match self {
            AudioRow::None => "None".to_string(),
            AudioRow::System => "Entire System".to_string(),
            AudioRow::App { label, .. } => label.clone(),
        }
    }

    fn to_share_audio(&self) -> ShareAudio {
        match self {
            AudioRow::None => ShareAudio::None,
            AudioRow::System => ShareAudio::System,
            AudioRow::App { node, .. } => ShareAudio::App { node: node.clone() },
        }
    }
}

/// Bitrate presets shown in the picker drop-down.
const BITRATE_PRESETS: &[(u32, &str)] = &[
    (2500, "2.5 Mbps"),
    (5000, "5 Mbps"),
    (6000, "6 Mbps"),
    (8000, "8 Mbps"),
    (12000, "12 Mbps"),
];

/// Return the index in `BITRATE_PRESETS` closest to `kbps` (by absolute distance).
fn bitrate_preset_index(kbps: u32) -> u32 {
    BITRATE_PRESETS
        .iter()
        .enumerate()
        .min_by_key(|(_, &(preset, _))| preset.abs_diff(kbps))
        .map(|(i, _)| i as u32)
        .unwrap_or(2) // default to 6 Mbps (index 2)
}

pub struct ScreenSharePicker {
    /// Currently selected source (Screen or Window).
    source: ShareSource,
    width: u32,
    height: u32,
    fps: u32,
    content: ContentType,
    bitrate_kbps: u32,
    audio_rows: Vec<AudioRow>,
    audio_idx: u32,
    pipewire_available: bool,
    /// Widgets for res/fps/content radio groups – held so `apply_settings` can
    /// activate the correct button without going through a message round-trip.
    /// The `SignalHandlerId` per entry lets us block the toggled handler during
    /// programmatic activation so no spurious `ConfigChanged` is emitted.
    res_btns: Vec<(u32, u32, gtk::ToggleButton, gtk::glib::SignalHandlerId)>,
    fps_btns: Vec<(u32, gtk::ToggleButton, gtk::glib::SignalHandlerId)>,
    content_btns: Vec<(ContentType, gtk::ToggleButton, gtk::glib::SignalHandlerId)>,
    audio_dropdown: gtk::DropDown,
    audio_handler: gtk::glib::SignalHandlerId,
    bitrate_dropdown: gtk::DropDown,
    bitrate_handler: gtk::glib::SignalHandlerId,
    bitrate_spin: gtk::SpinButton,
    bitrate_spin_handler: gtk::glib::SignalHandlerId,
    /// The FlowBox holding the source cards.
    source_flow: gtk::FlowBox,
    /// Parallel list of card widgets for highlighting; index 0 = "Whole screen".
    source_cards: Vec<gtk::Box>,
    /// Parallel list of Picture widgets for async thumbnail delivery; index 0 = "Whole screen".
    thumb_pics: Vec<gtk::Picture>,
    /// Index into `source_cards` that is currently highlighted.
    selected_idx: usize,
    /// "Running" flag for the current thumbnail worker. Flip to `false` to stop it.
    thumb_worker: Arc<AtomicBool>,
}

#[derive(Debug)]
pub enum PickerInput {
    /// Source card clicked (carries the card's index in the grid).
    SelectSourceAt { idx: usize, source: ShareSource },
    /// Resolution radio toggled (only fires when it becomes active).
    SelectResolution(u32, u32),
    /// FPS radio toggled.
    SelectFps(u32),
    /// Content-type radio toggled.
    SelectContent(ContentType),
    /// Audio drop-down selection changed.
    SelectAudio(u32),
    /// Bitrate drop-down selection changed (carries the preset index).
    SelectBitrate(u32),
    /// Bitrate spin-button changed (carries the raw kbps value).
    SelectBitrateCustom(u32),
    /// Rebuild the window list in the source grid (called on each picker open).
    SetWindows(Vec<ShareWindow>),
    /// Refresh the per-app audio node list (called on each picker open).
    SetAudioNodes(Vec<engine::screen::audio::AudioNode>),
    /// Apply persisted settings: update model fields + button state atomically.
    ApplySettings { width: u32, height: u32, fps: u32, content: ContentType, audio: ShareAudio, bitrate_kbps: u32 },
    /// Background worker delivers a captured thumbnail.
    ThumbnailReady { index: usize, png: Vec<u8> },
    GoLive,
    Cancel,
}

#[relm4::component(pub)]
impl SimpleComponent for ScreenSharePicker {
    /// The root passes in a `gtk::Picture` it owns; this component embeds it.
    /// The non-`Send` paintable is set on the picture by the root directly,
    /// never through a message.
    type Init = gtk::Picture;
    type Input = PickerInput;
    type Output = PickerOutput;

    view! {
        gtk::Window {
            set_title: Some("Share your screen"),
            set_default_width: 820,
            set_default_height: 600,
            set_modal: true,
            set_resizable: true,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 12,
                set_margin_all: 16,

                // ── Source grid ──────────────────────────────────────────────
                gtk::Label {
                    set_label: "Select a source",
                    set_xalign: 0.0,
                    add_css_class: "heading",
                },

                gtk::ScrolledWindow {
                    set_hexpand: true,
                    set_height_request: 160,
                    set_policy: (gtk::PolicyType::Automatic, gtk::PolicyType::Automatic),

                    #[local_ref]
                    source_flow -> gtk::FlowBox {
                        set_homogeneous: true,
                        set_row_spacing: 8,
                        set_column_spacing: 8,
                        set_max_children_per_line: 6,
                        set_min_children_per_line: 2,
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },

                // ── Preview ───────────────────────────────────────────────────
                gtk::Label {
                    set_label: "Preview",
                    set_xalign: 0.0,
                    add_css_class: "heading",
                },

                gtk::Frame {
                    set_hexpand: true,
                    set_vexpand: true,

                    #[local_ref]
                    picture -> gtk::Picture {
                        set_hexpand: true,
                        set_vexpand: true,
                    },
                },

                // ── Quality row ───────────────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 24,
                    set_homogeneous: true,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 4,

                        gtk::Label { set_label: "Resolution", set_xalign: 0.0 },

                        #[local_ref]
                        res_box -> gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 4,

                        gtk::Label { set_label: "Frame Rate", set_xalign: 0.0 },

                        #[local_ref]
                        fps_box -> gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 4,

                        gtk::Label { set_label: "Content Type", set_xalign: 0.0 },

                        #[local_ref]
                        content_box -> gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 4,
                        },
                    },
                },

                // ── Audio row ─────────────────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 8,
                    set_valign: gtk::Align::Center,

                    gtk::Label { set_label: "Audio Source:" },

                    #[local_ref]
                    audio_dropdown -> gtk::DropDown {},
                },

                // ── Bitrate row ───────────────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 8,
                    set_valign: gtk::Align::Center,

                    gtk::Label { set_label: "Bitrate:" },

                    #[local_ref]
                    bitrate_dropdown -> gtk::DropDown {},

                    #[local_ref]
                    bitrate_spin -> gtk::SpinButton {},

                    gtk::Label { set_label: "kbps" },
                },

                // ── Action row ────────────────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 8,
                    set_halign: gtk::Align::End,

                    gtk::Button {
                        set_label: "Cancel",
                        connect_clicked[sender] => move |_| {
                            let _ = sender.input(PickerInput::Cancel);
                        },
                    },

                    gtk::Button {
                        set_label: "Go Live",
                        add_css_class: "suggested-action",
                        connect_clicked[sender] => move |_| {
                            let _ = sender.input(PickerInput::GoLive);
                        },
                    },
                },
            },
        }
    }

    fn init(
        picture: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let pipewire_available = has_pipewire();
        let app_nodes = list_app_nodes();
        let windows = list_windows();

        // ── Audio rows ──────────────────────────────────────────────────────
        let mut audio_rows = vec![AudioRow::None, AudioRow::System];
        for n in &app_nodes {
            audio_rows.push(AudioRow::App { node: n.node.clone(), label: n.label.clone() });
        }

        let audio_labels: Vec<String> = audio_rows.iter().map(|r| r.label()).collect();
        let audio_string_list = gtk::StringList::new(
            &audio_labels.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let audio_dropdown = gtk::DropDown::new(Some(audio_string_list), gtk::Expression::NONE);

        if !pipewire_available {
            audio_dropdown.set_sensitive(false);
            audio_dropdown.set_tooltip_text(Some("PipeWire not available – audio capture disabled"));
        }

        // ── Source FlowBox + initial cards ──────────────────────────────────
        let source_flow = gtk::FlowBox::new();
        source_flow.set_homogeneous(true);
        source_flow.set_row_spacing(8);
        source_flow.set_column_spacing(8);
        source_flow.set_max_children_per_line(6);
        source_flow.set_min_children_per_line(2);
        source_flow.set_selection_mode(gtk::SelectionMode::None);

        let mut source_cards: Vec<gtk::Box> = Vec::new();
        let mut thumb_pics: Vec<gtk::Picture> = Vec::new();

        // Build the initial source list for the thumbnail worker.
        let mut init_sources: Vec<(usize, ShareSource)> = Vec::new();

        // "Whole screen" card at index 0.
        let (whole_card, whole_pic) = build_source_card("Whole screen", true);
        attach_card_click(&whole_card, 0, ShareSource::Screen { monitor: 0 }, &sender);
        source_flow.append(&whole_card);
        source_cards.push(whole_card);
        thumb_pics.push(whole_pic);
        init_sources.push((0, ShareSource::Screen { monitor: 0 }));

        for (i, w) in windows.iter().enumerate() {
            let xid = w.xid;
            let (card, pic) = build_source_card(&w.title, false);
            attach_card_click(&card, i + 1, ShareSource::Window { xid }, &sender);
            source_flow.append(&card);
            source_cards.push(card);
            thumb_pics.push(pic);
            init_sources.push((i + 1, ShareSource::Window { xid }));
        }

        drop(windows);

        // ── Resolution buttons ───────────────────────────────────────────────
        let res_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let res_presets: &[(u32, u32, &str)] = &[
            (854, 480, "480p"),
            (1280, 720, "720p"),
            (1920, 1080, "1080p"),
            (2560, 1440, "1440p"),
            (3840, 2160, "4K"),
        ];

        // Physical pixels = logical geometry × scale factor.
        let (native_w, native_h) = gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_then(|obj| obj.downcast::<gtk::gdk::Monitor>().ok())
            .map(|m| {
                let geo = m.geometry();
                let scale = m.scale_factor();
                (geo.width() as u32 * scale as u32, geo.height() as u32 * scale as u32)
            })
            .unwrap_or((u32::MAX, u32::MAX));

        let visible_presets: Vec<(u32, u32, &str)> = {
            let filtered: Vec<_> =
                res_presets.iter().copied().filter(|&(w, h, _)| w <= native_w && h <= native_h).collect();

            if filtered.is_empty() {
                vec![*res_presets.first().expect("res_presets is non-empty")]
            } else {
                filtered
            }
        };

        let mut res_btns: Vec<(u32, u32, gtk::ToggleButton, gtk::glib::SignalHandlerId)> = Vec::new();
        let mut first_res: Option<gtk::ToggleButton> = None;

        let default_res_w = if visible_presets.iter().any(|&(w, _, _)| w == 1920) {
            1920u32
        } else {
            visible_presets.last().map(|&(w, _, _)| w).unwrap_or(1920)
        };

        for &(w, h, label) in &visible_presets {
            let btn = gtk::ToggleButton::with_label(label);

            if let Some(first) = &first_res {
                btn.set_group(Some(first));
            } else {
                first_res = Some(btn.clone());
            }

            if w == default_res_w {
                btn.set_active(true);
            }

            let sender = sender.clone();
            let handler = btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectResolution(w, h));
                }
            });
            res_box.append(&btn);
            res_btns.push((w, h, btn, handler));
        }

        // ── FPS buttons ──────────────────────────────────────────────────────
        let fps_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let fps_presets: &[(u32, &str)] = &[(15, "15"), (30, "30"), (60, "60")];
        let mut fps_btns: Vec<(u32, gtk::ToggleButton, gtk::glib::SignalHandlerId)> = Vec::new();
        let mut first_fps: Option<gtk::ToggleButton> = None;
        for &(fps, label) in fps_presets {
            let btn = gtk::ToggleButton::with_label(label);

            if let Some(first) = &first_fps {
                btn.set_group(Some(first));
            } else {
                first_fps = Some(btn.clone());
            }

            if fps == 30 {
                btn.set_active(true);
            }

            let sender = sender.clone();
            let handler = btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectFps(fps));
                }
            });
            fps_box.append(&btn);
            fps_btns.push((fps, btn, handler));
        }

        // ── Content-type buttons ─────────────────────────────────────────────
        let content_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let content_presets: &[(ContentType, &str)] =
            &[(ContentType::Smoothness, "Smoothness"), (ContentType::Clarity, "Clarity")];
        let mut content_btns: Vec<(ContentType, gtk::ToggleButton, gtk::glib::SignalHandlerId)> = Vec::new();
        let mut first_ct: Option<gtk::ToggleButton> = None;
        for &(ct, label) in content_presets {
            let btn = gtk::ToggleButton::with_label(label);

            if let Some(first) = &first_ct {
                btn.set_group(Some(first));
            } else {
                first_ct = Some(btn.clone());
            }

            if ct == ContentType::Smoothness {
                btn.set_active(true);
            }

            let sender = sender.clone();
            let handler = btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectContent(ct));
                }
            });
            content_box.append(&btn);
            content_btns.push((ct, btn, handler));
        }

        // ── Audio selection change ────────────────────────────────────────────
        let audio_handler = {
            let sender = sender.clone();
            audio_dropdown.connect_selected_notify(move |dd| {
                let _ = sender.input(PickerInput::SelectAudio(dd.selected()));
            })
        };

        // ── Bitrate drop-down ─────────────────────────────────────────────────
        let bitrate_labels: Vec<&str> = BITRATE_PRESETS.iter().map(|(_, l)| *l).collect();
        let bitrate_string_list = gtk::StringList::new(&bitrate_labels);
        let bitrate_dropdown = gtk::DropDown::new(Some(bitrate_string_list), gtk::Expression::NONE);

        // Default to the 6 Mbps preset (index 2).
        bitrate_dropdown.set_selected(2);

        let bitrate_handler = {
            let sender = sender.clone();
            bitrate_dropdown.connect_selected_notify(move |dd| {
                let _ = sender.input(PickerInput::SelectBitrate(dd.selected()));
            })
        };

        // ── Bitrate spin-button ───────────────────────────────────────────────
        // min 500 kbps, max 50 000 kbps, step 250, page 1000.
        let bitrate_adj =
            gtk::Adjustment::new(6000.0, 500.0, 50_000.0, 250.0, 1000.0, 0.0);
        let bitrate_spin = gtk::SpinButton::new(Some(&bitrate_adj), 250.0, 0);

        let bitrate_spin_handler = {
            let sender = sender.clone();
            bitrate_spin.connect_value_changed(move |spin| {
                let _ = sender.input(PickerInput::SelectBitrateCustom(spin.value() as u32));
            })
        };

        let default_res_h = visible_presets
            .iter()
            .find(|&&(w, _, _)| w == default_res_w)
            .map(|&(_, h, _)| h)
            .unwrap_or(1080);

        // Dead flag – replaced immediately by spawn_thumb_worker below.
        let thumb_worker = Arc::new(AtomicBool::new(false));

        let mut model = ScreenSharePicker {
            source: ShareSource::Screen { monitor: 0 },
            width: default_res_w,
            height: default_res_h,
            fps: 30,
            content: ContentType::Smoothness,
            bitrate_kbps: 6000,
            audio_rows,
            audio_idx: 0,
            pipewire_available,
            res_btns,
            fps_btns,
            content_btns,
            audio_dropdown: audio_dropdown.clone(),
            audio_handler,
            bitrate_dropdown: bitrate_dropdown.clone(),
            bitrate_handler,
            bitrate_spin: bitrate_spin.clone(),
            bitrate_spin_handler,
            source_flow: source_flow.clone(),
            source_cards,
            thumb_pics,
            selected_idx: 0,
            thumb_worker,
        };

        model.spawn_thumb_worker(&sender, init_sources);

        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            PickerInput::SelectSourceAt { idx, source } => {
                self.highlight_card(idx);
                self.source = source;
                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SelectResolution(w, h) => {
                self.width = w;
                self.height = h;
                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SelectFps(fps) => {
                self.fps = fps;
                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SelectContent(ct) => {
                self.content = ct;
                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SelectAudio(idx) => {
                self.audio_idx = idx;
                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SelectBitrate(idx) => {
                if let Some(&(kbps, _)) = BITRATE_PRESETS.get(idx as usize) {
                    self.bitrate_kbps = kbps;

                    // Mirror the preset value into the spin-button without re-firing its handler.
                    self.bitrate_spin.block_signal(&self.bitrate_spin_handler);
                    self.bitrate_spin.set_value(kbps as f64);
                    self.bitrate_spin.unblock_signal(&self.bitrate_spin_handler);

                    let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
                }
            }
            PickerInput::SelectBitrateCustom(kbps) => {
                self.bitrate_kbps = kbps;

                // If the typed value matches a preset, reflect it in the dropdown.
                // Otherwise leave the dropdown where it is – no crash on non-matching value.
                if let Some(preset_idx) =
                    BITRATE_PRESETS.iter().position(|&(p, _)| p == kbps)
                {
                    self.bitrate_dropdown.block_signal(&self.bitrate_handler);
                    self.bitrate_dropdown.set_selected(preset_idx as u32);
                    self.bitrate_dropdown.unblock_signal(&self.bitrate_handler);
                }

                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SetWindows(windows) => {
                self.rebuild_source_cards(&windows, &sender);
                let _ = sender.output(PickerOutput::ConfigChanged(self.current_config()));
            }
            PickerInput::SetAudioNodes(nodes) => {
                self.rebuild_audio_rows(nodes);
            }
            PickerInput::ApplySettings { width, height, fps, content, audio, bitrate_kbps } => {
                self.apply_settings(width, height, fps, content, &audio, bitrate_kbps);
            }
            PickerInput::ThumbnailReady { index, png } => {
                if let Some(pic) = self.thumb_pics.get(index) {
                    let bytes = gtk::glib::Bytes::from(&png);
                    if let Ok(tex) = gtk::gdk::Texture::from_bytes(&bytes) {
                        pic.set_paintable(Some(&tex));
                    }
                }
            }
            PickerInput::GoLive => {
                self.thumb_worker.store(false, Ordering::Relaxed);
                let _ = sender.output(PickerOutput::GoLive(self.current_config()));
            }
            PickerInput::Cancel => {
                self.thumb_worker.store(false, Ordering::Relaxed);
                let _ = sender.output(PickerOutput::Cancel);
            }
        }
    }
}

/// Build a source card with a placeholder Picture. Returns both the outer card
/// `gtk::Box` and the inner `gtk::Picture` so thumbnails can be delivered later.
fn build_source_card(title: &str, selected: bool) -> (gtk::Box, gtk::Picture) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("source-card");

    if selected {
        card.add_css_class("selected");
    }

    let picture = gtk::Picture::new();
    picture.set_hexpand(true);
    picture.set_size_request(120, 68);
    picture.set_can_shrink(true);

    let label = gtk::Label::new(Some(title));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(18);
    label.add_css_class("source-card-title");

    card.append(&picture);
    card.append(&label);

    (card, picture)
}

/// Wire a click gesture on `card` that sends `SelectSourceAt { idx, source }`.
fn attach_card_click(
    card: &gtk::Box,
    idx: usize,
    source: ShareSource,
    sender: &ComponentSender<ScreenSharePicker>,
) {
    let sender = sender.clone();
    let gesture = gtk::GestureClick::new();
    gesture.connect_released(move |_, _, _, _| {
        let _ = sender.input(PickerInput::SelectSourceAt {
            idx,
            source: source.clone(),
        });
    });
    card.add_controller(gesture);
}

impl ScreenSharePicker {
    fn current_config(&self) -> ShareConfig {
        let audio = if !self.pipewire_available {
            ShareAudio::None
        } else {
            self.audio_rows
                .get(self.audio_idx as usize)
                .map(AudioRow::to_share_audio)
                .unwrap_or(ShareAudio::None)
        };

        ShareConfig {
            source: self.source.clone(),
            width: self.width,
            height: self.height,
            fps: self.fps,
            content: self.content,
            audio,
            bitrate_kbps: self.bitrate_kbps,
        }
    }

    /// Update the `.selected` CSS class to point at `idx`.
    fn highlight_card(&mut self, idx: usize) {
        if let Some(prev) = self.source_cards.get(self.selected_idx) {
            prev.remove_css_class("selected");
        }

        self.selected_idx = idx;

        if let Some(card) = self.source_cards.get(idx) {
            card.add_css_class("selected");
        }
    }

    /// Apply persisted settings to both the model fields and the UI controls.
    fn apply_settings(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
        content: ContentType,
        audio: &ShareAudio,
        bitrate_kbps: u32,
    ) {
        // Always reset to "Whole screen" so the source is deterministic.
        self.source = ShareSource::Screen { monitor: 0 };
        self.highlight_card(0);

        let (eff_w, eff_h) = if self.res_btns.iter().any(|(bw, bh, _, _)| *bw == width && *bh == height) {
            (width, height)
        } else {
            self.res_btns
                .last()
                .map(|&(bw, bh, _, _)| (bw, bh))
                .unwrap_or((width, height))
        };

        self.width = eff_w;
        self.height = eff_h;
        self.fps = fps;
        self.content = content;

        for (w, h, btn, handler) in &self.res_btns {
            if *w == eff_w && *h == eff_h {
                btn.block_signal(handler);
                btn.set_active(true);
                btn.unblock_signal(handler);
            }
        }

        for (f, btn, handler) in &self.fps_btns {
            if *f == fps {
                btn.block_signal(handler);
                btn.set_active(true);
                btn.unblock_signal(handler);
            }
        }

        for (ct, btn, handler) in &self.content_btns {
            if *ct == content {
                btn.block_signal(handler);
                btn.set_active(true);
                btn.unblock_signal(handler);
            }
        }

        let idx = self
            .audio_rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.to_share_audio() == *audio)
            .map(|(i, _)| i as u32)
            .unwrap_or(0);
        self.audio_idx = idx;
        self.audio_dropdown.block_signal(&self.audio_handler);
        self.audio_dropdown.set_selected(idx);
        self.audio_dropdown.unblock_signal(&self.audio_handler);

        self.bitrate_kbps = bitrate_kbps;

        let bitrate_idx = bitrate_preset_index(bitrate_kbps);
        self.bitrate_dropdown.block_signal(&self.bitrate_handler);
        self.bitrate_dropdown.set_selected(bitrate_idx);
        self.bitrate_dropdown.unblock_signal(&self.bitrate_handler);

        self.bitrate_spin.block_signal(&self.bitrate_spin_handler);
        self.bitrate_spin.set_value(bitrate_kbps as f64);
        self.bitrate_spin.unblock_signal(&self.bitrate_spin_handler);
    }

    /// Rebuild the audio dropdown from a freshly queried node list.
    fn rebuild_audio_rows(&mut self, nodes: Vec<engine::screen::audio::AudioNode>) {
        self.audio_rows = vec![AudioRow::None, AudioRow::System];

        for n in &nodes {
            self.audio_rows.push(AudioRow::App { node: n.node.clone(), label: n.label.clone() });
        }

        let labels: Vec<String> = self.audio_rows.iter().map(|r| r.label()).collect();
        let string_list = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());

        self.audio_dropdown.set_model(Some(&string_list));
        self.audio_idx = 0;
        self.audio_dropdown.set_selected(0);
    }

    /// Remove all cards and rebuild from the window list.
    /// "Whole screen" is always inserted first and highlighted.
    fn rebuild_source_cards(
        &mut self,
        windows: &[ShareWindow],
        sender: &ComponentSender<Self>,
    ) {
        while let Some(child) = self.source_flow.first_child() {
            self.source_flow.remove(&child);
        }
        self.source_cards.clear();
        self.thumb_pics.clear();

        let mut sources: Vec<(usize, ShareSource)> = Vec::new();

        // "Whole screen" at index 0.
        let (whole_card, whole_pic) = build_source_card("Whole screen", true);
        attach_card_click(&whole_card, 0, ShareSource::Screen { monitor: 0 }, sender);
        self.source_flow.append(&whole_card);
        self.source_cards.push(whole_card);
        self.thumb_pics.push(whole_pic);
        sources.push((0, ShareSource::Screen { monitor: 0 }));

        for (i, w) in windows.iter().enumerate() {
            let xid = w.xid;
            let (card, pic) = build_source_card(&w.title, false);
            attach_card_click(&card, i + 1, ShareSource::Window { xid }, sender);
            self.source_flow.append(&card);
            self.source_cards.push(card);
            self.thumb_pics.push(pic);
            sources.push((i + 1, ShareSource::Window { xid }));
        }

        // Reset selection to "Whole screen".
        self.selected_idx = 0;
        self.source = ShareSource::Screen { monitor: 0 };

        self.spawn_thumb_worker(sender, sources);
    }

    /// Stop any running worker and start a fresh one that captures thumbnails
    /// sequentially, delivering each via `ThumbnailReady`, then sleeps 4 s
    /// before repeating. At most one worker thread is alive at a time.
    fn spawn_thumb_worker(
        &mut self,
        sender: &ComponentSender<Self>,
        sources: Vec<(usize, ShareSource)>,
    ) {
        // Signal the previous worker to stop.
        self.thumb_worker.store(false, Ordering::Relaxed);

        let flag = Arc::new(AtomicBool::new(true));
        self.thumb_worker = Arc::clone(&flag);

        let sender = sender.clone();

        std::thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                for (index, src) in &sources {
                    if !flag.load(Ordering::Relaxed) {
                        break;
                    }

                    if let Some(png) = thumbnail(src, 240, 135) {
                        let _ = sender.input(PickerInput::ThumbnailReady { index: *index, png });
                    }
                }

                std::thread::sleep(Duration::from_secs(4));
            }
        });
    }
}
