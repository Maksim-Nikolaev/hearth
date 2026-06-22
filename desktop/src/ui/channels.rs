use gtk::prelude::*;
use relm4::prelude::*;

/// The channel rail: a static `# general` text channel and a `🔊 Voice` channel
/// you can join/leave, listing its current members beneath it.
pub struct Channels {
    in_voice: bool,
    voice_members: Vec<String>,
}

#[derive(Debug)]
pub enum ChannelsInput {
    SetVoice { in_voice: bool, members: Vec<String> },
}

#[derive(Debug)]
pub enum ChannelsOutput {
    /// Toggle voice membership; the parent owns the current state and flips it.
    ToggleVoice,
}

#[relm4::component(pub)]
impl SimpleComponent for Channels {
    type Init = ();
    type Input = ChannelsInput;
    type Output = ChannelsOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 2,

            gtk::Label {
                set_label: "TEXT",
                add_css_class: "section-header",
                set_xalign: 0.0,
            },
            gtk::Label {
                set_label: "# general",
                add_css_class: "channel",
                add_css_class: "channel-active",
                set_xalign: 0.0,
            },

            gtk::Label {
                set_label: "VOICE",
                add_css_class: "section-header",
                set_xalign: 0.0,
            },
            gtk::Button {
                #[watch]
                set_label: if model.in_voice { "🔊 Voice  (leave)" } else { "🔊 Voice  (join)" },
                add_css_class: "channel",
                connect_clicked[sender] => move |_| {
                    let _ = sender.output(ChannelsOutput::ToggleVoice);
                },
            },

            #[name = "voice_list"]
            gtk::Label {
                #[watch]
                set_label: &model.voice_members_text(),
                #[watch]
                set_visible: !model.voice_members.is_empty(),
                add_css_class: "member",
                add_css_class: "in-voice",
                set_xalign: 0.0,
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Channels { in_voice: false, voice_members: Vec::new() };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            ChannelsInput::SetVoice { in_voice, members } => {
                self.in_voice = in_voice;
                self.voice_members = members;
            }
        }
    }
}

impl Channels {
    fn voice_members_text(&self) -> String {
        self.voice_members.iter().map(|m| format!("   🔊 {m}")).collect::<Vec<_>>().join("\n")
    }
}
