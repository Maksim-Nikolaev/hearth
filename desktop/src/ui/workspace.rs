use gtk::prelude::*;
use hearth_protocol::{ChatEntry, PeerInfo};
use relm4::prelude::*;
use uuid::Uuid;

/// The 3-pane Discord-style container shown after login. Owns the workspace UI
/// state; the root feeds it `SessionEvent`-derived inputs and forwards its
/// outputs to the engine `Session`.
pub struct Workspace {
    online: Vec<PeerInfo>,
    voice: Vec<PeerInfo>,
    sharers: Vec<Uuid>,
    messages: Vec<ChatEntry>,
}

#[derive(Debug)]
pub enum WorkspaceInput {
    Roster(Vec<PeerInfo>),
    PeerJoined { user: Uuid, username: String },
    PeerLeft { user: Uuid },
    VoiceRoster(Vec<PeerInfo>),
    VoiceJoined { user: Uuid, username: String },
    VoiceLeft { user: Uuid },
    ShareStarted { user: Uuid },
    ShareStopped { user: Uuid },
    ChatHistory(Vec<ChatEntry>),
    Chat(ChatEntry),
}

#[relm4::component(pub)]
impl SimpleComponent for Workspace {
    type Init = ();
    type Input = WorkspaceInput;
    type Output = ();

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,

            // Left rail: channels + self-panel.
            gtk::Box {
                add_css_class: "rail",
                set_orientation: gtk::Orientation::Vertical,
                set_width_request: 220,

                gtk::Label {
                    set_label: "CHANNELS",
                    add_css_class: "section-header",
                    set_xalign: 0.0,
                    set_vexpand: true,
                    set_valign: gtk::Align::Start,
                },
            },

            // Center: stage + chat.
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_hexpand: true,

                gtk::Label {
                    #[watch]
                    set_label: &format!(
                        "Stage – {} sharing, {} online, {} in voice, {} messages",
                        model.sharers.len(), model.online.len(), model.voice.len(),
                        model.messages.len(),
                    ),
                    set_vexpand: true,
                },
            },

            // Right: members.
            gtk::Box {
                add_css_class: "rail",
                set_orientation: gtk::Orientation::Vertical,
                set_width_request: 220,

                gtk::Label {
                    set_label: "MEMBERS",
                    add_css_class: "section-header",
                    set_xalign: 0.0,
                    set_vexpand: true,
                    set_valign: gtk::Align::Start,
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Workspace {
            online: Vec::new(),
            voice: Vec::new(),
            sharers: Vec::new(),
            messages: Vec::new(),
        };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            WorkspaceInput::Roster(list) => self.online = list,
            WorkspaceInput::PeerJoined { user, username } => {
                if !self.online.iter().any(|p| p.user == user) {
                    self.online.push(PeerInfo { user, username });
                }
            }
            WorkspaceInput::PeerLeft { user } => {
                self.online.retain(|p| p.user != user);
                self.voice.retain(|p| p.user != user);
            }
            WorkspaceInput::VoiceRoster(list) => self.voice = list,
            WorkspaceInput::VoiceJoined { user, username } => {
                if !self.voice.iter().any(|p| p.user == user) {
                    self.voice.push(PeerInfo { user, username });
                }
            }
            WorkspaceInput::VoiceLeft { user } => self.voice.retain(|p| p.user != user),
            WorkspaceInput::ShareStarted { user } => {
                if !self.sharers.contains(&user) {
                    self.sharers.push(user);
                }
            }
            WorkspaceInput::ShareStopped { user } => self.sharers.retain(|u| *u != user),
            WorkspaceInput::ChatHistory(list) => self.messages = list,
            WorkspaceInput::Chat(entry) => self.messages.push(entry),
        }
    }
}
