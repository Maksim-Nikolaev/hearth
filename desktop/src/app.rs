use crate::config::Config;
use engine::flow::VideoSink;
use engine::session::{Connection, Presence, Session, SessionEvent};
use gtk::prelude::*;
use hearth_protocol::{ChatEntry, PeerInfo, ServerMessage};
use relm4::prelude::*;

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Login,
    Connecting,
    Room,
}

pub struct AppModel {
    config: Config,
    title: String,
    screen: Screen,
    status: String,
    session: Option<Session>,
    peers: Vec<PeerInfo>,
    messages: Vec<ChatEntry>,
    video_paintable: Option<gtk::gdk::Paintable>,
}

#[derive(Debug)]
pub enum AppMsg {
    Login { username: String, password: String },
    SendChat(String),
    ShareScreen,
    Call,
    Stop,
    Mute(bool),
    Deafen(bool),
}

/// Async/command results. Manual `Debug` because `Connection` is opaque.
pub enum Cmd {
    Opened(Connection),
    Failed(String),
    Server(ServerMessage),
    Event(SessionEvent),
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::Opened(_) => write!(f, "Opened"),
            Cmd::Failed(e) => write!(f, "Failed({e})"),
            Cmd::Server(_) => write!(f, "Server"),
            Cmd::Event(e) => write!(f, "Event({e:?})"),
        }
    }
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = Config;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = Cmd;

    view! {
        gtk::Window {
            set_title: Some(model.title.as_str()),
            set_default_width: 960,
            set_default_height: 640,

            gtk::Stack {
                set_margin_all: 24,

                add_named[Some("login")] = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    gtk::Label {
                        set_label: "Hearth",
                        add_css_class: "title-1",
                    },

                    #[name = "username_entry"]
                    gtk::Entry {
                        set_placeholder_text: Some("username"),
                        set_width_request: 240,
                    },

                    #[name = "password_entry"]
                    gtk::Entry {
                        set_placeholder_text: Some("password"),
                        set_visibility: false,
                        set_width_request: 240,
                    },

                    gtk::Button {
                        set_label: "Log in",
                        add_css_class: "suggested-action",
                        connect_clicked[sender, username_entry, password_entry] => move |_| {
                            sender.input(AppMsg::Login {
                                username: username_entry.text().to_string(),
                                password: password_entry.text().to_string(),
                            });
                        },
                    },

                    gtk::Label {
                        #[watch]
                        set_label: &model.status,
                    },
                },

                add_named[Some("connecting")] = &gtk::Box {
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    gtk::Label { set_label: "Connecting…" },
                },

                add_named[Some("room")] = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 8,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 6,

                        gtk::Button {
                            set_label: "Share screen",
                            connect_clicked => AppMsg::ShareScreen,
                        },
                        gtk::Button {
                            set_label: "Call",
                            connect_clicked => AppMsg::Call,
                        },
                        gtk::ToggleButton {
                            set_label: "Mute",
                            connect_toggled[sender] => move |b| sender.input(AppMsg::Mute(b.is_active())),
                        },
                        gtk::ToggleButton {
                            set_label: "Deafen",
                            connect_toggled[sender] => move |b| sender.input(AppMsg::Deafen(b.is_active())),
                        },
                        gtk::Button {
                            set_label: "Stop",
                            connect_clicked => AppMsg::Stop,
                        },
                        gtk::Label {
                            set_hexpand: true,
                            set_xalign: 1.0,
                            #[watch]
                            set_label: &model.status,
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 12,
                        set_vexpand: true,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 6,
                            set_width_request: 180,

                            gtk::Label { set_label: "Online", add_css_class: "heading", set_xalign: 0.0 },
                            gtk::Label {
                                #[watch]
                                set_label: &model.peers_text(),
                                set_xalign: 0.0,
                                set_valign: gtk::Align::Start,
                            },
                        },

                        gtk::Frame {
                            set_hexpand: true,
                            gtk::Picture {
                                set_vexpand: true,
                                set_hexpand: true,
                                #[watch]
                                set_paintable: model.video_paintable.as_ref(),
                            },
                        },

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 6,
                            set_width_request: 300,

                            gtk::ScrolledWindow {
                                set_vexpand: true,
                                gtk::Label {
                                    #[watch]
                                    set_label: &model.chat_text(),
                                    set_xalign: 0.0,
                                    set_valign: gtk::Align::End,
                                    set_wrap: true,
                                },
                            },

                            gtk::Entry {
                                set_placeholder_text: Some("Message…"),
                                connect_activate[sender] => move |entry| {
                                    let body = entry.text().to_string();
                                    if !body.is_empty() {
                                        sender.input(AppMsg::SendChat(body));
                                        entry.set_text("");
                                    }
                                },
                            },
                        },
                    },
                },

                // Set after the pages are added so the initial value always
                // resolves to an existing child (no startup warning).
                #[watch]
                set_visible_child_name: model.screen_name(),
            }
        }
    }

    fn init(config: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut screen = Screen::Login;

        if let Some(token) = config.load_token() {
            screen = Screen::Connecting;
            let ws = config.ws.clone();
            sender.oneshot_command(async move {
                match Session::open_with_token(&ws, &token).await {
                    Ok(c) => Cmd::Opened(c),
                    Err(e) => Cmd::Failed(e.to_string()),
                }
            });
        }

        let title = std::env::var("HEARTH_TITLE")
            .map(|t| format!("Hearth - {t}"))
            .unwrap_or_else(|_| "Hearth".into());

        let model = AppModel {
            config,
            title,
            screen,
            status: String::new(),
            session: None,
            peers: Vec::new(),
            messages: Vec::new(),
            video_paintable: None,
        };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            AppMsg::Login { username, password } => {
                self.screen = Screen::Connecting;
                self.status.clear();

                let http = self.config.http.clone();
                let ws = self.config.ws.clone();
                sender.oneshot_command(async move {
                    match Session::open(&http, &ws, &username, &password).await {
                        Ok(c) => Cmd::Opened(c),
                        Err(e) => Cmd::Failed(e.to_string()),
                    }
                });
            }
            AppMsg::SendChat(body) => {
                if let Some(s) = self.session.as_ref() {
                    s.send_chat(&body);
                }
            }
            AppMsg::ShareScreen => {
                if let Some(s) = self.session.as_mut() {
                    s.start_share();
                }
            }
            AppMsg::Call => self.with_first_peer(|s, peer| {
                if let Err(e) = s.start_call(peer) {
                    eprintln!("start_call: {e}");
                }
            }),
            AppMsg::Stop => {
                if let Some(s) = self.session.as_mut() {
                    s.stop_all();
                }
                self.video_paintable = None;
            }
            AppMsg::Mute(on) => {
                if let Some(s) = self.session.as_ref() {
                    s.mute(on);
                }
            }
            AppMsg::Deafen(on) => {
                if let Some(s) = self.session.as_ref() {
                    s.deafen(on);
                }
            }
        }
    }

    fn update_cmd(&mut self, msg: Self::CommandOutput, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            Cmd::Opened(conn) => {
                self.config.save_token(conn.token());

                let (session, mut inbound, mut events) = Session::start(conn, VideoSink::Paintable);
                session.join("main");
                self.session = Some(session);
                self.screen = Screen::Room;
                self.status = "Connected".into();

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            while let Some(m) = inbound.recv().await {
                                if out.send(Cmd::Server(m)).is_err() {
                                    break;
                                }
                            }
                        })
                        .drop_on_shutdown()
                });

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            while let Some(e) = events.recv().await {
                                if out.send(Cmd::Event(e)).is_err() {
                                    break;
                                }
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            Cmd::Failed(e) => {
                self.screen = Screen::Login;
                self.status = format!("Login failed: {e}");
            }
            Cmd::Server(m) => {
                if let Some(s) = self.session.as_mut() {
                    s.handle(m);
                }
            }
            Cmd::Event(e) => self.on_event(e),
        }
    }
}

impl AppModel {
    fn on_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Presence(Presence::Roster(list)) => self.peers = list,
            SessionEvent::Presence(Presence::Joined { user, username }) => {
                if !self.peers.iter().any(|p| p.user == user) {
                    self.peers.push(PeerInfo { user, username });
                }
            }
            SessionEvent::Presence(Presence::Left { user }) => self.peers.retain(|p| p.user != user),
            SessionEvent::Chat(entry) => self.messages.push(entry),
            SessionEvent::ChatHistory(list) => self.messages = list,
            SessionEvent::FlowState { flow, state, .. } => self.status = format!("{flow:?}: {state}"),
            SessionEvent::VideoReady { peer, flow } => {
                if let Some(obj) = self.session.as_ref().and_then(|s| s.paintable_for(peer, flow)) {
                    self.video_paintable = obj.downcast::<gtk::gdk::Paintable>().ok();
                }
            }
            SessionEvent::Error(e) => self.status = e,
            // Group voice + share events are consumed by the workspace components
            // introduced in M6 Tasks 5–7; ignored by this transitional shell.
            SessionEvent::VoiceState(_)
            | SessionEvent::VoiceJoined { .. }
            | SessionEvent::VoiceLeft { .. }
            | SessionEvent::ShareStarted { .. }
            | SessionEvent::ShareStopped { .. } => {}
        }
    }

    fn with_first_peer(&mut self, f: impl FnOnce(&mut Session, uuid::Uuid)) {
        let Some(peer) = self.peers.first().map(|p| p.user) else {
            self.status = "No peer in the room yet".into();
            return;
        };
        if let Some(s) = self.session.as_mut() {
            f(s, peer);
        }
    }

    fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Login => "login",
            Screen::Connecting => "connecting",
            Screen::Room => "room",
        }
    }

    fn peers_text(&self) -> String {
        if self.peers.is_empty() {
            return "(no one else here)".into();
        }
        self.peers.iter().map(|p| format!("● {}", p.username)).collect::<Vec<_>>().join("\n")
    }

    fn chat_text(&self) -> String {
        self.messages.iter().map(|m| format!("{}: {}", m.username, m.body)).collect::<Vec<_>>().join("\n")
    }
}
