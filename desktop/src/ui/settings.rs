use crate::config::{ActivationKind, NsLevel, Settings};
use crate::ui::meter::LevelBar;
use engine::audio::devices::{AudioDevice, DeviceKind};
use gtk::glib::SignalHandlerId;
use gtk::prelude::*;
use relm4::prelude::*;

// ── Output ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SettingsOutput {
    InputDevice(Option<String>),
    OutputDevice(Option<String>),
    InputVolume(f64),
    OutputVolume(f64),
    NoiseSuppression(NsLevel),
    EchoCancellation(bool),
    Agc(bool),
    Vad(bool),
    InputSensitivity(f32),
    Activation(ActivationKind),
    PttKey(Option<String>),
    JitterLatency(u32),
    MicTest(bool),
    ResetDefaults,
    /// Revert all settings to the snapshot taken when the window opened.
    Discard,
    /// Close (hide) the settings window. Settings already applied immediately.
    Close,
}

// ── Input ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SettingsInput {
    /// Populate device dropdowns from a fresh enumeration.
    SetDevices(Vec<AudioDevice>),
    /// Populate all controls from persisted settings.
    SetSettings(Settings),
    /// Forward a live input level reading to the embedded level bar.
    SetLevel(f32),
    /// Gate transmitting state — draws the mic-test "transmitting" glow.
    SetTransmitting(bool),
    /// Programmatically set the Mic Test toggle without re-emitting its handler.
    SetMicTestActive(bool),
}

// ── Model ─────────────────────────────────────────────────────────────────────

pub struct SettingsWindow {
    settings: Settings,
    mic_devices: Vec<AudioDevice>,
    spk_devices: Vec<AudioDevice>,
    level_bar: LevelBar,
    // Shared with the device-selection signal closures so repopulate can update
    // the list without reconnecting the signal.
    mic_devices_cell: std::rc::Rc<std::cell::RefCell<Vec<AudioDevice>>>,
    spk_devices_cell: std::rc::Rc<std::cell::RefCell<Vec<AudioDevice>>>,
}

// ── Widget struct (named fields the macro generates) ─────────────────────────

pub struct SettingsWindowWidgets {
    mic_dropdown: gtk::DropDown,
    spk_dropdown: gtk::DropDown,
    input_vol_scale: gtk::Scale,
    output_vol_scale: gtk::Scale,
    ns_dropdown: gtk::DropDown,
    ec_switch: gtk::Switch,
    agc_switch: gtk::Switch,
    vad_switch: gtk::Switch,
    activation_dropdown: gtk::DropDown,
    ptt_btn: gtk::Button,
    jitter_spin: gtk::SpinButton,
    mic_test_btn: gtk::ToggleButton,
    // Signal handler IDs – stored so programmatic updates can be blocked.
    mic_test_toggled_id: SignalHandlerId,
    mic_selected_id: SignalHandlerId,
    spk_selected_id: SignalHandlerId,
    input_vol_id: SignalHandlerId,
    output_vol_id: SignalHandlerId,
    ns_selected_id: SignalHandlerId,
    ec_active_id: SignalHandlerId,
    agc_active_id: SignalHandlerId,
    vad_active_id: SignalHandlerId,
    activation_selected_id: SignalHandlerId,
    jitter_value_id: SignalHandlerId,
}

// ── Component ─────────────────────────────────────────────────────────────────

impl Component for SettingsWindow {
    type Init = ();
    type Input = SettingsInput;
    type Output = SettingsOutput;
    type CommandOutput = ();
    type Root = gtk::Window;
    type Widgets = SettingsWindowWidgets;

    fn init_root() -> Self::Root {
        gtk::Window::builder()
            .title("Settings – Voice")
            .default_width(520)
            .default_height(620)
            .hide_on_close(true)
            .build()
    }

    fn init(
        _init: Self::Init,
        window: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // ── Build the widget tree manually ────────────────────────────────────
        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(20)
            .margin_bottom(20)
            .margin_start(20)
            .margin_end(20)
            .build();

        window.set_child(Some(&root_box));

        // Closing via the window's X (hide_on_close handles the hide) still needs
        // to resume audio — emit Close so the app restores mute/deafen.
        {
            let s = sender.clone();
            window.connect_close_request(move |_| {
                let _ = s.output(SettingsOutput::Close);
                gtk::glib::Propagation::Proceed
            });
        }

        // Devices section
        root_box.append(&section_label("DEVICES"));

        let mic_dropdown = gtk::DropDown::from_strings(&["Default"]);
        mic_dropdown.set_hexpand(true);
        root_box.append(&hrow("Microphone", 140, &mic_dropdown));

        let spk_dropdown = gtk::DropDown::from_strings(&["Default"]);
        spk_dropdown.set_hexpand(true);
        root_box.append(&hrow("Speaker", 140, &spk_dropdown));

        // Volume section
        root_box.append(&section_label("VOLUME"));

        let input_vol_scale =
            gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
        input_vol_scale.set_hexpand(true);
        input_vol_scale.set_draw_value(true);
        input_vol_scale.set_value_pos(gtk::PositionType::Right);
        input_vol_scale.set_value(1.0);
        root_box.append(&hrow("Mic volume", 140, &input_vol_scale));

        let output_vol_scale =
            gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
        output_vol_scale.set_hexpand(true);
        output_vol_scale.set_draw_value(true);
        output_vol_scale.set_value_pos(gtk::PositionType::Right);
        output_vol_scale.set_value(1.0);
        root_box.append(&hrow("Speaker vol.", 140, &output_vol_scale));

        // Mic test + input sensitivity section
        root_box.append(&section_label("MIC TEST & SENSITIVITY"));

        // The LevelBar emits InputSensitivity whenever the user drags the handle.
        let level_bar = {
            let s = sender.clone();
            LevelBar::new(-40.0, move |db| {
                let _ = s.output(SettingsOutput::InputSensitivity(db));
            })
        };

        let mic_test_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();

        let mic_test_btn = gtk::ToggleButton::with_label("Test Mic");
        level_bar.drawing_area.set_valign(gtk::Align::Center);
        mic_test_row.append(&mic_test_btn);
        mic_test_row.append(&level_bar.drawing_area);
        root_box.append(&mic_test_row);

        // DSP section
        root_box.append(&section_label("AUDIO PROCESSING"));

        let ns_dropdown =
            gtk::DropDown::from_strings(&["Off", "Low", "Moderate", "High"]);
        ns_dropdown.set_selected(2); // Moderate default
        ns_dropdown.set_tooltip_text(Some(
            "Removes background noise (RNNoise). May add ~10 ms latency.",
        ));
        root_box.append(&hrow("Noise suppression", 180, &ns_dropdown));

        let ec_switch = gtk::Switch::new();
        ec_switch.set_active(true);
        ec_switch.set_tooltip_text(Some(
            "Cancels speaker echo picked up by the mic — not needed with a headset, and not yet implemented on the native path.",
        ));
        root_box.append(&hrow("Echo cancellation", 180, &ec_switch));

        let agc_switch = gtk::Switch::new();
        agc_switch.set_active(true);
        agc_switch.set_tooltip_text(Some(
            "Auto-levels your mic so your voice stays a consistent loudness. May add a little latency.",
        ));
        root_box.append(&hrow("Auto gain control", 180, &agc_switch));

        let vad_switch = gtk::Switch::new();
        vad_switch.set_active(true);
        vad_switch.set_tooltip_text(Some(
            "Detects speech vs. noise to gate transmission in Voice-activity mode. May add a little latency.",
        ));
        root_box.append(&hrow("Voice activity det.", 180, &vad_switch));

        // Activation section
        root_box.append(&section_label("ACTIVATION"));

        let activation_dropdown =
            gtk::DropDown::from_strings(&["Voice activity", "Push-to-talk", "Always on"]);
        root_box.append(&hrow("Mode", 140, &activation_dropdown));

        // The bind field is itself the capture control: click it, then press the
        // key or mouse button to bind. No free-text entry.
        let ptt_btn = gtk::Button::with_label("Click to bind");
        ptt_btn.set_hexpand(true);
        ptt_btn.set_tooltip_text(Some(
            "Click, then press a key or mouse button (incl. side buttons) to bind for push-to-talk",
        ));
        root_box.append(&hrow("PTT bind", 140, &ptt_btn));

        // Network section
        root_box.append(&section_label("NETWORK"));

        // Jitter buffer depth (ms). Lower = less latency, more sensitive to
        // network jitter. Applies to the GStreamer fallback transport; the native
        // voice path (default on Windows) uses its own fixed mixer lane.
        let jitter_spin = gtk::SpinButton::with_range(0.0, 500.0, 5.0);
        jitter_spin.set_value(20.0);
        root_box.append(&hrow("Jitter buffer (ms)", 180, &jitter_spin));

        // Bottom action row. Settings apply immediately (no Save); "Discard"
        // reverts to the values as they were when the window opened.
        let reset_btn = gtk::Button::with_label("Reset to Defaults");
        let discard_btn = gtk::Button::with_label("Discard Changes");
        let close_btn = gtk::Button::with_label("Close");
        close_btn.add_css_class("suggested-action");
        let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        action_row.set_halign(gtk::Align::End);
        action_row.set_margin_top(12);
        action_row.append(&reset_btn);
        action_row.append(&discard_btn);
        action_row.append(&close_btn);
        root_box.append(&action_row);

        // ── Wire signals (once; handler IDs stored for block/unblock) ─────────

        // Device dropdowns – the closure captures a shared cell so repopulate can
        // swap the device list without reconnecting the signal.
        let mic_devices_cell: std::rc::Rc<std::cell::RefCell<Vec<AudioDevice>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let spk_devices_cell: std::rc::Rc<std::cell::RefCell<Vec<AudioDevice>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

        let mic_selected_id = {
            let s = sender.clone();
            let cell = mic_devices_cell.clone();
            mic_dropdown.connect_selected_notify(move |dd| {
                let idx = dd.selected() as usize;
                let id = if idx == 0 {
                    None
                } else {
                    cell.borrow().get(idx - 1).map(|d| d.id.clone())
                };
                let _ = s.output(SettingsOutput::InputDevice(id));
            })
        };

        let spk_selected_id = {
            let s = sender.clone();
            let cell = spk_devices_cell.clone();
            spk_dropdown.connect_selected_notify(move |dd| {
                let idx = dd.selected() as usize;
                let id = if idx == 0 {
                    None
                } else {
                    cell.borrow().get(idx - 1).map(|d| d.id.clone())
                };
                let _ = s.output(SettingsOutput::OutputDevice(id));
            })
        };

        let input_vol_id = {
            let s = sender.clone();
            input_vol_scale.connect_value_changed(move |sc| {
                let _ = s.output(SettingsOutput::InputVolume(sc.value()));
            })
        };

        let output_vol_id = {
            let s = sender.clone();
            output_vol_scale.connect_value_changed(move |sc| {
                let _ = s.output(SettingsOutput::OutputVolume(sc.value()));
            })
        };

        let mic_test_toggled_id = {
            let s = sender.clone();
            mic_test_btn.connect_toggled(move |b| {
                let _ = s.output(SettingsOutput::MicTest(b.is_active()));
            })
        };

        let ns_selected_id = {
            let s = sender.clone();
            ns_dropdown.connect_selected_notify(move |dd| {
                let level = match dd.selected() {
                    0 => NsLevel::Off,
                    1 => NsLevel::Low,
                    2 => NsLevel::Moderate,
                    _ => NsLevel::High,
                };
                let _ = s.output(SettingsOutput::NoiseSuppression(level));
            })
        };

        let ec_active_id = {
            let s = sender.clone();
            ec_switch.connect_active_notify(move |sw| {
                let _ = s.output(SettingsOutput::EchoCancellation(sw.is_active()));
            })
        };

        let agc_active_id = {
            let s = sender.clone();
            agc_switch.connect_active_notify(move |sw| {
                let _ = s.output(SettingsOutput::Agc(sw.is_active()));
            })
        };

        let vad_active_id = {
            let s = sender.clone();
            vad_switch.connect_active_notify(move |sw| {
                let _ = s.output(SettingsOutput::Vad(sw.is_active()));
            })
        };

        // The PTT bind field is only editable in Push-to-talk mode.
        ptt_btn.set_sensitive(activation_dropdown.selected() == 1);
        let activation_selected_id = {
            let s = sender.clone();
            let ptt = ptt_btn.clone();
            activation_dropdown.connect_selected_notify(move |dd| {
                let kind = match dd.selected() {
                    0 => ActivationKind::Voice,
                    1 => ActivationKind::PushToTalk,
                    _ => ActivationKind::AlwaysOn,
                };
                ptt.set_sensitive(matches!(kind, ActivationKind::PushToTalk));
                let _ = s.output(SettingsOutput::Activation(kind));
            })
        };

        // PTT capture: clicking the field arms a one-shot listener; the next key
        // OR mouse button (caught in the capture phase, before any widget) becomes
        // the bind. `keyval.name()` is the X11/GDK key name (identical on
        // X11/Wayland/Windows); mouse buttons are stored as "Mouse<N>" (X11-style:
        // 8=back, 9=forward, 2=middle). The engine maps both to the platform.
        {
            let armed = std::rc::Rc::new(std::cell::Cell::new(false));

            // Arm on click.
            {
                let armed = armed.clone();
                let btn = ptt_btn.clone();
                ptt_btn.connect_clicked(move |_| {
                    armed.set(true);
                    btn.set_label("Press a key or mouse button…");
                });
            }

            // Keyboard capture.
            {
                let key_ctl = gtk::EventControllerKey::new();
                key_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
                let armed = armed.clone();
                let btn = ptt_btn.clone();
                let s = sender.clone();
                key_ctl.connect_key_pressed(move |_, keyval, _, _| {
                    if !armed.get() {
                        return gtk::glib::Propagation::Proceed;
                    }
                    if let Some(name) = keyval.name() {
                        btn.set_label(name.as_str());
                        let _ = s.output(SettingsOutput::PttKey(Some(name.to_string())));
                    }
                    armed.set(false);
                    gtk::glib::Propagation::Stop
                });
                root_box.add_controller(key_ctl);
            }

            // Mouse-button capture (incl. side buttons). 0 = listen for any button.
            {
                let click = gtk::GestureClick::new();
                click.set_button(0);
                click.set_propagation_phase(gtk::PropagationPhase::Capture);
                let armed = armed.clone();
                let btn = ptt_btn.clone();
                let s = sender.clone();
                click.connect_pressed(move |gesture, _, _, _| {
                    if !armed.get() {
                        return; // not armed — let the click (e.g. arming click) pass
                    }
                    let name = format!("Mouse{}", gesture.current_button());
                    btn.set_label(&name);
                    let _ = s.output(SettingsOutput::PttKey(Some(name)));
                    armed.set(false);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                });
                root_box.add_controller(click);
            }
        }

        let jitter_value_id = {
            let s = sender.clone();
            jitter_spin.connect_value_changed(move |sb| {
                let _ = s.output(SettingsOutput::JitterLatency(sb.value() as u32));
            })
        };

        {
            let s = sender.clone();
            reset_btn.connect_clicked(move |_| {
                let _ = s.output(SettingsOutput::ResetDefaults);
            });
        }

        {
            let s = sender.clone();
            discard_btn.connect_clicked(move |_| {
                let _ = s.output(SettingsOutput::Discard);
            });
        }

        {
            let s = sender.clone();
            close_btn.connect_clicked(move |_| {
                let _ = s.output(SettingsOutput::Close);
            });
        }

        let model = SettingsWindow {
            settings: Settings::default(),
            mic_devices: Vec::new(),
            spk_devices: Vec::new(),
            level_bar,
            mic_devices_cell,
            spk_devices_cell,
        };

        let widgets = SettingsWindowWidgets {
            mic_dropdown,
            spk_dropdown,
            input_vol_scale,
            output_vol_scale,
            ns_dropdown,
            ec_switch,
            agc_switch,
            vad_switch,
            activation_dropdown,
            ptt_btn,
            jitter_spin,
            mic_test_btn,
            mic_test_toggled_id,
            mic_selected_id,
            spk_selected_id,
            input_vol_id,
            output_vol_id,
            ns_selected_id,
            ec_active_id,
            agc_active_id,
            vad_active_id,
            activation_selected_id,
            jitter_value_id,
        };

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            SettingsInput::SetDevices(devices) => {
                self.mic_devices = devices.iter()
                    .filter(|d| d.kind == DeviceKind::Source)
                    .cloned()
                    .collect();

                self.spk_devices = devices.iter()
                    .filter(|d| d.kind == DeviceKind::Sink)
                    .cloned()
                    .collect();

                self.repopulate_dropdowns(widgets);
            }

            SettingsInput::SetSettings(s) => {
                self.settings = s.clone();
                self.apply_settings_to_widgets(widgets, &s);
            }

            SettingsInput::SetLevel(db) => {
                self.level_bar.set_level(db);
            }
            SettingsInput::SetTransmitting(on) => {
                self.level_bar.set_transmitting(on);
            }

            SettingsInput::SetMicTestActive(active) => {
                widgets.mic_test_btn.block_signal(&widgets.mic_test_toggled_id);
                widgets.mic_test_btn.set_active(active);
                widgets.mic_test_btn.unblock_signal(&widgets.mic_test_toggled_id);
            }
        }
    }

    fn update(
        &mut self,
        _msg: Self::Input,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        // All logic is in update_with_view; this is a no-op required by the trait.
    }
}

impl SettingsWindow {
    /// Rebuild the mic/speaker `StringList` models and restore the saved selection.
    /// The selection signals are blocked during all programmatic writes to prevent
    /// spurious outputs.
    fn repopulate_dropdowns(&self, widgets: &mut SettingsWindowWidgets) {
        let mic_labels: Vec<&str> = std::iter::once("Default")
            .chain(self.mic_devices.iter().map(|d| d.label.as_str()))
            .collect();

        let spk_labels: Vec<&str> = std::iter::once("Default")
            .chain(self.spk_devices.iter().map(|d| d.label.as_str()))
            .collect();

        let mic_idx = self.settings.input_device
            .as_deref()
            .and_then(|id| self.mic_devices.iter().position(|d| d.id == id))
            .map(|i| i + 1)
            .unwrap_or(0) as u32;

        let spk_idx = self.settings.output_device
            .as_deref()
            .and_then(|id| self.spk_devices.iter().position(|d| d.id == id))
            .map(|i| i + 1)
            .unwrap_or(0) as u32;

        // Update the device lists read by the live signal closures.
        *self.mic_devices_cell.borrow_mut() = self.mic_devices.clone();
        *self.spk_devices_cell.borrow_mut() = self.spk_devices.clone();

        // Block signals so set_model / set_selected do not emit outputs.
        widgets.mic_dropdown.block_signal(&widgets.mic_selected_id);
        widgets.spk_dropdown.block_signal(&widgets.spk_selected_id);

        widgets.mic_dropdown.set_model(Some(&gtk::StringList::new(&mic_labels)));
        widgets.spk_dropdown.set_model(Some(&gtk::StringList::new(&spk_labels)));
        widgets.mic_dropdown.set_selected(mic_idx);
        widgets.spk_dropdown.set_selected(spk_idx);

        widgets.mic_dropdown.unblock_signal(&widgets.mic_selected_id);
        widgets.spk_dropdown.unblock_signal(&widgets.spk_selected_id);
    }

    /// Sync every non-device-dropdown widget to the given settings snapshot.
    /// All signals are blocked so loading saved settings emits no outputs.
    /// The level bar threshold is updated silently via `set_threshold_silent`.
    fn apply_settings_to_widgets(&self, widgets: &mut SettingsWindowWidgets, s: &Settings) {
        widgets.input_vol_scale.block_signal(&widgets.input_vol_id);
        widgets.output_vol_scale.block_signal(&widgets.output_vol_id);
        widgets.ns_dropdown.block_signal(&widgets.ns_selected_id);
        widgets.ec_switch.block_signal(&widgets.ec_active_id);
        widgets.agc_switch.block_signal(&widgets.agc_active_id);
        widgets.vad_switch.block_signal(&widgets.vad_active_id);
        widgets.activation_dropdown.block_signal(&widgets.activation_selected_id);
        widgets.jitter_spin.block_signal(&widgets.jitter_value_id);

        widgets.input_vol_scale.set_value(s.input_volume);
        widgets.jitter_spin.set_value(s.jitter_latency_ms as f64);
        widgets.output_vol_scale.set_value(s.output_volume);

        // Threshold is on the level bar, not a GTK signal – set silently.
        self.level_bar.set_threshold_silent(s.input_sensitivity);

        let ns_idx = match s.noise_suppression {
            NsLevel::Off => 0,
            NsLevel::Low => 1,
            NsLevel::Moderate => 2,
            NsLevel::High => 3,
        };
        widgets.ns_dropdown.set_selected(ns_idx);

        widgets.ec_switch.set_active(s.echo_cancellation);
        widgets.agc_switch.set_active(s.agc);
        widgets.vad_switch.set_active(s.vad);

        let act_idx = match s.activation {
            ActivationKind::Voice => 0,
            ActivationKind::PushToTalk => 1,
            ActivationKind::AlwaysOn => 2,
        };
        widgets.activation_dropdown.set_selected(act_idx);
        widgets.ptt_btn.set_sensitive(matches!(s.activation, ActivationKind::PushToTalk));

        widgets.ptt_btn.set_label(s.ptt_key.as_deref().unwrap_or("Click to bind"));

        widgets.input_vol_scale.unblock_signal(&widgets.input_vol_id);
        widgets.output_vol_scale.unblock_signal(&widgets.output_vol_id);
        widgets.ns_dropdown.unblock_signal(&widgets.ns_selected_id);
        widgets.ec_switch.unblock_signal(&widgets.ec_active_id);
        widgets.agc_switch.unblock_signal(&widgets.agc_active_id);
        widgets.vad_switch.unblock_signal(&widgets.vad_active_id);
        widgets.activation_dropdown.unblock_signal(&widgets.activation_selected_id);
        widgets.jitter_spin.unblock_signal(&widgets.jitter_value_id);
    }
}

// ── Layout helpers ────────────────────────────────────────────────────────────

fn section_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("section-header");
    l
}

/// A horizontal row with a fixed-width label on the left and a widget on the right.
fn hrow(label: &str, label_width: i32, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let lbl = gtk::Label::new(Some(label));
    lbl.set_width_request(label_width);
    lbl.set_xalign(0.0);

    row.append(&lbl);
    row.append(widget);

    row
}
