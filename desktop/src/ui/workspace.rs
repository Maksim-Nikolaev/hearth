use crate::ui::channels::{Channels, ChannelsInput, ChannelsOutput};
use crate::ui::chat::{Chat, ChatInput, ChatOutput};
use crate::ui::members::{Members, MembersInput, MemberRowData};
use crate::ui::self_panel::{SelfPanel, SelfPanelInput, SelfPanelOutput};
use crate::ui::stage::{Stage, StageInput, StageOutput};
use gtk::prelude::*;
use hearth_protocol::{ChatEntry, PeerInfo};
use relm4::prelude::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use uuid::Uuid;

/// Received screen paintables per sharer, shared between the root (which fills it
/// from `SessionEvent::VideoReady`) and the workspace (which shows the selected
/// one). Shared by `Rc` rather than sent in a message, since a `Paintable` is not
/// `Send` and all relm4 messages here must be.
pub type Screens = Rc<RefCell<HashMap<Uuid, gtk::gdk::Paintable>>>;

/// The 3-pane Discord-style container shown after login. Owns the workspace UI
/// state and the per-pane child components; the root feeds it `SessionEvent`-
/// derived inputs and forwards its outputs to the engine `Session`.
pub struct Workspace {
    self_id: Uuid,
    self_name: String,
    in_voice: bool,
    /// True when the local user is actively sharing their screen.
    sharing: bool,
    online: Vec<PeerInfo>,
    voice: Vec<PeerInfo>,
    sharers: Vec<Uuid>,
    selected: Option<Uuid>,
    screens: Screens,
    picture: gtk::Picture,
    channels: Controller<Channels>,
    self_panel: Controller<SelfPanel>,
    members: Controller<Members>,
    stage: Controller<Stage>,
    chat: Controller<Chat>,
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
    /// A screen paintable arrived in the shared map; re-sync the stage.
    VideoReady,
    // Bubbled up from child panes:
    ToggleVoice,
    Mute(bool),
    Deafen(bool),
    OpenSharePicker,
    StopShare,
    SelectSharer(Uuid),
    SendChat(String),
    OpenSettings,
    /// Notify the workspace that the local share is now live (true) or stopped
    /// (false). Updates the Share button style and the self members-row marker.
    SetSharing(bool),
}

#[derive(Debug)]
pub enum WorkspaceOutput {
    JoinVoice,
    LeaveVoice,
    Mute(bool),
    Deafen(bool),
    OpenSharePicker,
    StopShare,
    SendChat(String),
    OpenSettings,
}

#[relm4::component(pub)]
impl SimpleComponent for Workspace {
    type Init = Screens;
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

            // Center: stage (top) + chat (below).
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_hexpand: true,

                #[local_ref]
                stage_widget -> gtk::Box {},

                #[local_ref]
                chat_widget -> gtk::Box {
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
        screens: Self::Init,
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
                SelfPanelOutput::OpenSharePicker => WorkspaceInput::OpenSharePicker,
                SelfPanelOutput::StopShare => WorkspaceInput::StopShare,
                SelfPanelOutput::OpenSettings => WorkspaceInput::OpenSettings,
            });

        let members = Members::builder().launch(()).detach();

        let picture = gtk::Picture::new();

        let stage = Stage::builder()
            .launch(picture.clone())
            .forward(sender.input_sender(), |out| match out {
                StageOutput::Select(id) => WorkspaceInput::SelectSharer(id),
            });

        let chat = Chat::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                ChatOutput::Send(body) => WorkspaceInput::SendChat(body),
            });

        let model = Workspace {
            self_id: Uuid::nil(),
            self_name: String::new(),
            in_voice: false,
            sharing: false,
            online: Vec::new(),
            voice: Vec::new(),
            sharers: Vec::new(),
            selected: None,
            screens,
            picture,
            channels,
            self_panel,
            members,
            stage,
            chat,
        };

        let channels_widget = model.channels.widget();
        let self_panel_widget = model.self_panel.widget();
        let members_widget = model.members.widget();
        let stage_widget = model.stage.widget();
        let chat_widget = model.chat.widget();

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

                // A crashed sharer may never send ShareStopped; remove their
                // entry from the sharer list and screen map so the stage clears.
                self.sharers.retain(|u| *u != user);
                self.screens.borrow_mut().remove(&user);
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
            WorkspaceInput::ShareStopped { user } => {
                self.sharers.retain(|u| *u != user);
                self.screens.borrow_mut().remove(&user);
            }
            WorkspaceInput::ChatHistory(list) => {
                let _ = self.chat.sender().send(ChatInput::Reset(list));
            }
            WorkspaceInput::Chat(entry) => {
                let _ = self.chat.sender().send(ChatInput::Append(entry));
            }
            WorkspaceInput::VideoReady => {}
            WorkspaceInput::SelectSharer(id) => self.selected = Some(id),
            WorkspaceInput::SendChat(body) => {
                let _ = sender.output(WorkspaceOutput::SendChat(body));
            }
            WorkspaceInput::ToggleVoice => {
                self.in_voice = !self.in_voice;
                if self.in_voice {
                    let _ = sender.output(WorkspaceOutput::JoinVoice);
                } else {
                    self.voice.clear();
                    self.sharing = false;
                    // Leaving the call clears the stage: stop watching anyone and
                    // drop their (now-frozen) frames so no black box lingers.
                    self.sharers.clear();
                    self.screens.borrow_mut().clear();
                    self.selected = None;
                    let _ = self.self_panel.sender().send(SelfPanelInput::SetShareActive(false));
                    let _ = sender.output(WorkspaceOutput::LeaveVoice);
                }
            }
            WorkspaceInput::Mute(b) => {
                let _ = sender.output(WorkspaceOutput::Mute(b));
            }
            WorkspaceInput::Deafen(b) => {
                let _ = sender.output(WorkspaceOutput::Deafen(b));
            }
            WorkspaceInput::OpenSharePicker => {
                let _ = sender.output(WorkspaceOutput::OpenSharePicker);
            }
            WorkspaceInput::StopShare => {
                let _ = sender.output(WorkspaceOutput::StopShare);
            }
            WorkspaceInput::OpenSettings => {
                let _ = sender.output(WorkspaceOutput::OpenSettings);
            }
            WorkspaceInput::SetSharing(active) => {
                self.sharing = active;
                let _ = self.self_panel.sender().send(SelfPanelInput::SetShareActive(active));
            }
        }

        self.refresh();
        self.sync_stage();
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

    /// Push the active sharer set and the selected stream to the stage. Excludes
    /// our own share (we don't watch ourselves) and hides the stage when nobody
    /// is sharing, so chat fills the center.
    fn sync_stage(&mut self) {
        let watchable: Vec<Uuid> =
            self.sharers.iter().copied().filter(|id| *id != self.self_id).collect();

        if let Some(sel) = self.selected {
            if !watchable.contains(&sel) {
                self.selected = None;
            }
        }
        if self.selected.is_none() {
            self.selected = watchable.first().copied();
        }

        let tabs: Vec<(Uuid, String)> =
            watchable.iter().map(|id| (*id, self.name_of(*id))).collect();
        let _ = self.stage.sender().send(StageInput::SetSharers(tabs));

        let paintable = self.selected.and_then(|id| self.screens.borrow().get(&id).cloned());
        self.picture.set_paintable(paintable.as_ref());

        self.stage.widget().set_visible(!watchable.is_empty());
    }

    fn name_of(&self, id: Uuid) -> String {
        if id == self.self_id {
            return self.self_name.clone();
        }
        self.voice
            .iter()
            .chain(self.online.iter())
            .find(|p| p.user == id)
            .map(|p| p.username.clone())
            .unwrap_or_else(|| id.to_string())
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
                let is_self = *id == self.self_id;
                let label = if is_self && self.sharing {
                    format!("🔴 {}{}", name, you(*id))
                } else {
                    format!("🔊 {}{}", name, you(*id))
                };
                rows.push(member(label, true, is_self && self.sharing));
            }
        }

        if !online.is_empty() {
            rows.push(header("ONLINE"));
            for (id, name) in &online {
                rows.push(member(format!("● {}{}", name, you(*id)), false, false));
            }
        }

        rows
    }
}

fn header(label: &str) -> MemberRowData {
    MemberRowData { label: label.to_string(), is_header: true, in_voice: false, sharing: false }
}

fn member(label: String, in_voice: bool, sharing: bool) -> MemberRowData {
    MemberRowData { label, is_header: false, in_voice, sharing }
}
