use gtk::prelude::*;
use gtk::glib::SignalHandlerId;
use relm4::prelude::*;

/// The bottom-of-rail self panel: your name plus mute / deafen / share toggles.
pub struct SelfPanel {
    username: String,
    /// Signal id for the Share toggle's `toggled` handler. Stored so the handler
    /// can be blocked during programmatic `set_active` calls, preventing the
    /// toggle from firing `OpenSharePicker` or `StopShare` when we reset it.
    share_toggle_handler: Option<SignalHandlerId>,
    share_toggle: gtk::ToggleButton,
}

#[derive(Debug)]
pub enum SelfPanelInput {
    SetUsername(String),
    /// Programmatically update the Share toggle's visual state without triggering
    /// the `toggled` handler (and therefore without opening the picker again).
    SetShareActive(bool),
}

#[derive(Debug)]
pub enum SelfPanelOutput {
    Mute(bool),
    Deafen(bool),
    /// Share toggle turned on – open the picker.
    OpenSharePicker,
    /// Share toggle turned off – stop the active share.
    StopShare,
    OpenSettings,
}

pub struct SelfPanelWidgets {
    root: gtk::Box,
}

impl SimpleComponent for SelfPanel {
    type Init = ();
    type Input = SelfPanelInput;
    type Output = SelfPanelOutput;
    type Root = gtk::Box;
    type Widgets = SelfPanelWidgets;

    fn init_root() -> Self::Root {
        gtk::Box::new(gtk::Orientation::Vertical, 4)
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.set_margin_all(8);

        let name_label = gtk::Label::new(Some(""));
        name_label.add_css_class("self-name");
        name_label.set_xalign(0.0);
        root.append(&name_label);

        let button_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        button_row.set_homogeneous(true);

        let mute_btn = gtk::ToggleButton::with_label("Mute");
        {
            let sender = sender.clone();
            mute_btn.connect_toggled(move |b| {
                let _ = sender.output(SelfPanelOutput::Mute(b.is_active()));
            });
        }
        button_row.append(&mute_btn);

        let deafen_btn = gtk::ToggleButton::with_label("Deafen");
        {
            let sender = sender.clone();
            deafen_btn.connect_toggled(move |b| {
                let _ = sender.output(SelfPanelOutput::Deafen(b.is_active()));
            });
        }
        button_row.append(&deafen_btn);
        root.append(&button_row);

        let share_toggle = gtk::ToggleButton::with_label("Share screen");

        // Connect the handler manually so we can store the SignalHandlerId and
        // block it when we need to reset the toggle without side effects.
        let share_handler = {
            let sender = sender.clone();
            share_toggle.connect_toggled(move |b| {
                if b.is_active() {
                    let _ = sender.output(SelfPanelOutput::OpenSharePicker);
                } else {
                    let _ = sender.output(SelfPanelOutput::StopShare);
                }
            })
        };

        root.append(&share_toggle);

        let settings_btn = gtk::Button::with_label("Settings");
        {
            let sender = sender.clone();
            settings_btn.connect_clicked(move |_| {
                let _ = sender.output(SelfPanelOutput::OpenSettings);
            });
        }
        root.append(&settings_btn);

        let model = SelfPanel {
            username: String::new(),
            share_toggle_handler: Some(share_handler),
            share_toggle,
        };

        let widgets = SelfPanelWidgets { root };

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            SelfPanelInput::SetUsername(name) => {
                self.username = name;
            }
            SelfPanelInput::SetShareActive(active) => {
                // Block the signal so the programmatic set_active does not
                // re-emit OpenSharePicker or StopShare.
                if let Some(id) = &self.share_toggle_handler {
                    self.share_toggle.block_signal(id);
                    self.share_toggle.set_active(active);
                    self.share_toggle.unblock_signal(id);
                }

                if active {
                    self.share_toggle.set_label("● Sharing (click to stop)");
                    self.share_toggle.add_css_class("sharing");
                } else {
                    self.share_toggle.set_label("Share screen");
                    self.share_toggle.remove_css_class("sharing");
                }
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        // Update the name label on every render pass that follows a SetUsername.
        if let Some(child) = widgets.root.first_child() {
            if let Ok(label) = child.downcast::<gtk::Label>() {
                label.set_label(&self.username);
            }
        }
    }
}
