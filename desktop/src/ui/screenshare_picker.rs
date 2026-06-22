use engine::screen::audio::{has_pipewire, list_app_nodes, ShareAudio};
use engine::screen::sources::{list_windows, ContentType, ShareConfig, ShareSource, ShareWindow};
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

pub struct ScreenSharePicker {
    /// Currently selected source (Screen or Window).
    source: ShareSource,
    width: u32,
    height: u32,
    fps: u32,
    content: ContentType,
    audio_rows: Vec<AudioRow>,
    audio_idx: u32,
    pipewire_available: bool,
    /// Widgets for res/fps/content radio groups – held so `apply_settings` can
    /// activate the correct button without going through a message round-trip.
    res_btns: Vec<(u32, u32, gtk::ToggleButton)>,
    fps_btns: Vec<(u32, gtk::ToggleButton)>,
    content_btns: Vec<(ContentType, gtk::ToggleButton)>,
    audio_dropdown: gtk::DropDown,
    /// The source row – held so window buttons can be rebuilt on re-open.
    source_box: gtk::Box,
    /// The "Whole screen" anchor button, needed to set the group on new window buttons.
    whole_btn: gtk::ToggleButton,
}

#[derive(Debug)]
pub enum PickerInput {
    /// Source button clicked.
    SelectSource(ShareSource),
    /// Resolution radio toggled (only fires when it becomes active).
    SelectResolution(u32, u32),
    /// FPS radio toggled.
    SelectFps(u32),
    /// Content-type radio toggled.
    SelectContent(ContentType),
    /// Audio drop-down selection changed.
    SelectAudio(u32),
    /// Rebuild the window list in the source grid (called on each picker open).
    SetWindows(Vec<ShareWindow>),
    /// Refresh the per-app audio node list (called on each picker open).
    SetAudioNodes(Vec<engine::screen::audio::AudioNode>),
    /// Apply persisted settings: update model fields + button state atomically.
    ApplySettings { width: u32, height: u32, fps: u32, content: ContentType, audio: ShareAudio },
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
            set_default_height: 560,
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
                    set_height_request: 100,
                    set_policy: (gtk::PolicyType::Automatic, gtk::PolicyType::Never),

                    #[local_ref]
                    source_box -> gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 8,
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

        // ── Source box ──────────────────────────────────────────────────────
        let source_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let whole_btn = gtk::ToggleButton::with_label("Whole screen");
        whole_btn.set_active(true);
        {
            let sender = sender.clone();
            whole_btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectSource(
                        ShareSource::Screen { monitor: 0 },
                    ));
                }
            });
        }
        source_box.append(&whole_btn);

        for w in &windows {
            let btn = gtk::ToggleButton::with_label(&w.title);
            btn.set_group(Some(&whole_btn));
            let xid = w.xid;
            let sender = sender.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectSource(ShareSource::Window { xid }));
                }
            });
            source_box.append(&btn);
        }

        // Drop `windows` – it is no longer needed after the buttons are built.
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
        let mut res_btns: Vec<(u32, u32, gtk::ToggleButton)> = Vec::new();
        let mut first_res: Option<gtk::ToggleButton> = None;
        for &(w, h, label) in res_presets {
            let btn = gtk::ToggleButton::with_label(label);

            if let Some(first) = &first_res {
                btn.set_group(Some(first));
            } else {
                first_res = Some(btn.clone());
            }

            if w == 1920 {
                btn.set_active(true);
            }

            let sender = sender.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectResolution(w, h));
                }
            });
            res_box.append(&btn);
            res_btns.push((w, h, btn));
        }

        // ── FPS buttons ──────────────────────────────────────────────────────
        let fps_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let fps_presets: &[(u32, &str)] = &[(15, "15"), (30, "30"), (60, "60")];
        let mut fps_btns: Vec<(u32, gtk::ToggleButton)> = Vec::new();
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
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectFps(fps));
                }
            });
            fps_box.append(&btn);
            fps_btns.push((fps, btn));
        }

        // ── Content-type buttons ─────────────────────────────────────────────
        let content_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let content_presets: &[(ContentType, &str)] =
            &[(ContentType::Smoothness, "Smoothness"), (ContentType::Clarity, "Clarity")];
        let mut content_btns: Vec<(ContentType, gtk::ToggleButton)> = Vec::new();
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
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectContent(ct));
                }
            });
            content_box.append(&btn);
            content_btns.push((ct, btn));
        }

        // ── Audio selection change ────────────────────────────────────────────
        {
            let sender = sender.clone();
            audio_dropdown.connect_selected_notify(move |dd| {
                let _ = sender.input(PickerInput::SelectAudio(dd.selected()));
            });
        }

        let model = ScreenSharePicker {
            source: ShareSource::Screen { monitor: 0 },
            width: 1920,
            height: 1080,
            fps: 30,
            content: ContentType::Smoothness,
            audio_rows,
            audio_idx: 0,
            pipewire_available,
            res_btns,
            fps_btns,
            content_btns,
            audio_dropdown: audio_dropdown.clone(),
            source_box: source_box.clone(),
            whole_btn: whole_btn.clone(),
        };

        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            PickerInput::SelectSource(src) => {
                self.source = src;
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
            PickerInput::SetWindows(windows) => {
                self.rebuild_window_buttons(&windows, &sender);
            }
            PickerInput::SetAudioNodes(nodes) => {
                self.rebuild_audio_rows(nodes);
            }
            PickerInput::ApplySettings { width, height, fps, content, audio } => {
                self.apply_settings(width, height, fps, content, &audio);
            }
            PickerInput::GoLive => {
                let _ = sender.output(PickerOutput::GoLive(self.current_config()));
            }
            PickerInput::Cancel => {
                let _ = sender.output(PickerOutput::Cancel);
            }
        }
    }
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
        }
    }

    /// Apply persisted settings to both the model fields and the UI controls.
    /// Setting model fields directly ensures `current_config()` is correct even
    /// if the user clicks Go Live without touching any control (a button that is
    /// already active does not re-emit `toggled`).
    fn apply_settings(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
        content: ContentType,
        audio: &ShareAudio,
    ) {
        // Always reset to "Whole screen" so the source is deterministic.
        self.source = ShareSource::Screen { monitor: 0 };

        self.width = width;
        self.height = height;
        self.fps = fps;
        self.content = content;

        for (w, h, btn) in &self.res_btns {
            if *w == width && *h == height {
                btn.set_active(true);
            }
        }

        for (f, btn) in &self.fps_btns {
            if *f == fps {
                btn.set_active(true);
            }
        }

        for (ct, btn) in &self.content_btns {
            if *ct == content {
                btn.set_active(true);
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
        self.audio_dropdown.set_selected(idx);
    }

    /// Rebuild the audio dropdown from a freshly queried node list.
    ///
    /// Always keeps "None" and "Entire System" as the first two entries, then
    /// appends the current per-app nodes. The selected index is reset to 0
    /// (None) so stale app references are never silently carried over.
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

    /// Remove all window buttons from the source grid and rebuild them from the
    /// supplied list. The "Whole screen" button is always kept as the first entry.
    fn rebuild_window_buttons(&self, windows: &[ShareWindow], sender: &ComponentSender<Self>) {
        // Remove all children after `whole_btn`.
        while let Some(child) = self.source_box.last_child() {
            if child == self.whole_btn {
                break;
            }

            self.source_box.remove(&child);
        }

        for w in windows {
            let btn = gtk::ToggleButton::with_label(&w.title);
            btn.set_group(Some(&self.whole_btn));
            let xid = w.xid;
            let sender = sender.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.input(PickerInput::SelectSource(ShareSource::Window { xid }));
                }
            });
            self.source_box.append(&btn);
        }
    }
}
