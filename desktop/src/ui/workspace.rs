use crate::ui::channels::{Channels, ChannelsInput, ChannelsOutput};
use crate::ui::members::{Members, MembersInput, MemberRowData};
use crate::ui::self_panel::{SelfPanel, SelfPanelInput, SelfPanelOutput};
use gtk::prelude::*;
use hearth_protocol::{ChatEntry, PeerInfo};
use relm4::prelude::*;
use std::collections::HashSet;
use uuid::Uuid;

/// The 3-pane Discord-style container shown after login. Owns the workspace UI
/// state and the per-pane child components; the root feeds it `SessionEvent`-
/// derived inputs and forwards its outputs to the engine `Session`.
pub struct Workspace {
    self_id: Uuid,
    self_name: String,
    in_voice: bool,
    online: Vec<PeerInfo>,
    voice: Vec<PeerInfo>,
    sharers: Vec<Uuid>,
    messages: Vec<ChatEntry>,
    channels: Controller<Channels>,
    self_panel: Controller<SelfPanel>,
    members: Controller<Members>,
}

#[derive(Debug)]
pub enum WorkspaceInput {
    SetSelf { id: Uuid, username: String },
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
    // Bubbled up from child panes:
    ToggleVoice,
    Mute(bool),
    Deafen(bool),
    Share(bool),
}

#[derive(Debug)]
pub enum WorkspaceOutput {
    JoinVoice,
    LeaveVoice,
    Mute(bool),
    Deafen(bool),
    StartShare,
    StopShare,
}

#[relm4::component(pub)]
impl SimpleComponent for Workspace {
    type Init = ();
    type Input = WorkspaceInput;
    type Output = WorkspaceOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,

            // Left rail: channels (top) + self-panel (pinned bottom).
            gtk::Box {
                add_css_class: "rail",
                set_orientation: gtk::Orientation::Vertical,
                set_width_request: 220,

                #[local_ref]
                channels_widget -> gtk::Box {
                    set_vexpand: true,
                    set_valign: gtk::Align::Start,
                    set_margin_all: 8,
                },

                #[local_ref]
                self_panel_widget -> gtk::Box {},
            },

            // Center: stage + chat (placeholder until Task 7).
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_hexpand: true,

                gtk::Label {
                    #[watch]
                    set_label: &format!(
                        "Stage – {} sharing, {} messages",
                        model.sharers.len(), model.messages.len(),
                    ),
                    set_vexpand: true,
                },
            },

            // Right: members rail.
            gtk::Box {
                add_css_class: "rail",
                set_orientation: gtk::Orientation::Vertical,
                set_width_request: 220,

                #[local_ref]
                members_widget -> gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_margin_all: 8,
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let channels = Channels::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                ChannelsOutput::ToggleVoice => WorkspaceInput::ToggleVoice,
            });

        let self_panel = SelfPanel::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                SelfPanelOutput::Mute(b) => WorkspaceInput::Mute(b),
                SelfPanelOutput::Deafen(b) => WorkspaceInput::Deafen(b),
                SelfPanelOutput::Share(b) => WorkspaceInput::Share(b),
            });

        let members = Members::builder().launch(()).detach();

        let model = Workspace {
            self_id: Uuid::nil(),
            self_name: String::new(),
            in_voice: false,
            online: Vec::new(),
            voice: Vec::new(),
            sharers: Vec::new(),
            messages: Vec::new(),
            channels,
            self_panel,
            members,
        };

        let channels_widget = model.channels.widget();
        let self_panel_widget = model.self_panel.widget();
        let members_widget = model.members.widget();

        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            WorkspaceInput::SetSelf { id, username } => {
                self.self_id = id;
                self.self_name = username.clone();
                let _ = self.self_panel.sender().send(SelfPanelInput::SetUsername(username));
            }
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
            WorkspaceInput::ToggleVoice => {
                self.in_voice = !self.in_voice;
                if self.in_voice {
                    let _ = sender.output(WorkspaceOutput::JoinVoice);
                } else {
                    self.voice.clear();
                    let _ = sender.output(WorkspaceOutput::LeaveVoice);
                }
            }
            WorkspaceInput::Mute(b) => {
                let _ = sender.output(WorkspaceOutput::Mute(b));
            }
            WorkspaceInput::Deafen(b) => {
                let _ = sender.output(WorkspaceOutput::Deafen(b));
            }
            WorkspaceInput::Share(b) => {
                let _ = sender.output(if b {
                    WorkspaceOutput::StartShare
                } else {
                    WorkspaceOutput::StopShare
                });
            }
        }

        self.refresh();
    }
}

impl Workspace {
    /// Push the merged member roster to the members rail and the voice roster to
    /// the channels rail. Called after any state change.
    fn refresh(&self) {
        let _ = self.members.sender().send(MembersInput::SetRows(self.member_rows()));

        let mut voice_names: Vec<String> =
            self.voice.iter().map(|p| p.username.clone()).collect();
        if self.in_voice {
            voice_names.push(format!("{} (you)", self.self_name));
        }
        let _ = self.channels.sender().send(ChannelsInput::SetVoice {
            in_voice: self.in_voice,
            members: voice_names,
        });
    }

    fn member_rows(&self) -> Vec<MemberRowData> {
        let you = |id: Uuid| if id == self.self_id { " (you)" } else { "" };

        let mut in_voice: Vec<(Uuid, String)> =
            self.voice.iter().map(|p| (p.user, p.username.clone())).collect();
        if self.in_voice {
            in_voice.push((self.self_id, self.self_name.clone()));
        }

        let voice_ids: HashSet<Uuid> = in_voice.iter().map(|(id, _)| *id).collect();

        let mut online: Vec<(Uuid, String)> = self
            .online
            .iter()
            .filter(|p| !voice_ids.contains(&p.user))
            .map(|p| (p.user, p.username.clone()))
            .collect();
        if !self.in_voice {
            online.push((self.self_id, self.self_name.clone()));
        }

        let mut rows = Vec::new();

        if !in_voice.is_empty() {
            rows.push(header("IN VOICE"));
            for (id, name) in &in_voice {
                rows.push(member(format!("🔊 {}{}", name, you(*id)), true));
            }
        }

        if !online.is_empty() {
            rows.push(header("ONLINE"));
            for (id, name) in &online {
                rows.push(member(format!("● {}{}", name, you(*id)), false));
            }
        }

        rows
    }
}

fn header(label: &str) -> MemberRowData {
    MemberRowData { label: label.to_string(), is_header: true, in_voice: false }
}

fn member(label: String, in_voice: bool) -> MemberRowData {
    MemberRowData { label, is_header: false, in_voice }
}
