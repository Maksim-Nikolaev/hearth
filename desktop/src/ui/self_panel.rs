use gtk::prelude::*;
use relm4::prelude::*;

/// The bottom-of-rail self panel: your name plus mute / deafen / share toggles.
pub struct SelfPanel {
    username: String,
}

#[derive(Debug)]
pub enum SelfPanelInput {
    SetUsername(String),
}

#[derive(Debug)]
pub enum SelfPanelOutput {
    Mute(bool),
    Deafen(bool),
    Share(bool),
    OpenSettings,
}

#[relm4::component(pub)]
impl SimpleComponent for SelfPanel {
    type Init = ();
    type Input = SelfPanelInput;
    type Output = SelfPanelOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 4,
            set_margin_all: 8,

            gtk::Label {
                #[watch]
                set_label: &model.username,
                add_css_class: "self-name",
                set_xalign: 0.0,
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 4,
                set_homogeneous: true,

                gtk::ToggleButton {
                    set_label: "Mute",
                    connect_toggled[sender] => move |b| {
                        let _ = sender.output(SelfPanelOutput::Mute(b.is_active()));
                    },
                },
                gtk::ToggleButton {
                    set_label: "Deafen",
                    connect_toggled[sender] => move |b| {
                        let _ = sender.output(SelfPanelOutput::Deafen(b.is_active()));
                    },
                },
            },

            gtk::ToggleButton {
                set_label: "Share screen",
                connect_toggled[sender] => move |b| {
                    let _ = sender.output(SelfPanelOutput::Share(b.is_active()));
                },
            },

            gtk::Button {
                set_label: "Settings",
                connect_clicked[sender] => move |_| {
                    let _ = sender.output(SelfPanelOutput::OpenSettings);
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = SelfPanel { username: String::new() };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            SelfPanelInput::SetUsername(name) => self.username = name,
        }
    }
}
