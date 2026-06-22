use crate::config::Config;
use crate::ui::login::{LoginForm, LoginInput, LoginOutput};
use crate::ui::workspace::{Screens, Workspace, WorkspaceInput, WorkspaceOutput};
use engine::flow::VideoSink;
use engine::session::{Connection, Presence, Session, SessionEvent};
use gtk::prelude::*;
use hearth_protocol::ServerMessage;
use relm4::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Login,
    Connecting,
    Workspace,
}

pub struct AppModel {
    config: Config,
    title: String,
    screen: Screen,
    session: Option<Session>,
    screens: Screens,
    login: Controller<LoginForm>,
    workspace: Controller<Workspace>,
}

#[derive(Debug)]
pub enum AppMsg {
    Login { username: String, password: String },
    Workspace(WorkspaceOutput),
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
            set_default_width: 1100,
            set_default_height: 720,

            gtk::Stack {
                add_named[Some("login")]: login_widget,

                add_named[Some("connecting")] = &gtk::Box {
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    gtk::Label { set_label: "Connecting…" },
                },

                add_named[Some("workspace")]: workspace_widget,

                // Set after the pages exist so the initial value resolves to a
                // real child (no startup warning).
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

        let login = LoginForm::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                LoginOutput::Submit { username, password } => AppMsg::Login { username, password },
            });

        let screens: Screens = Rc::new(RefCell::new(HashMap::new()));

        let workspace = Workspace::builder()
            .launch(screens.clone())
            .forward(sender.input_sender(), AppMsg::Workspace);

        let model = AppModel {
            config,
            title,
            screen,
            session: None,
            screens,
            login,
            workspace,
        };

        let login_widget = model.login.widget();
        let workspace_widget = model.workspace.widget();

        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            AppMsg::Login { username, password } => {
                self.screen = Screen::Connecting;
                let _ = self.login.sender().send(LoginInput::SetStatus(String::new()));

                let http = self.config.http.clone();
                let ws = self.config.ws.clone();
                sender.oneshot_command(async move {
                    match Session::open(&http, &ws, &username, &password).await {
                        Ok(c) => Cmd::Opened(c),
                        Err(e) => Cmd::Failed(e.to_string()),
                    }
                });
            }
            AppMsg::Workspace(out) => {
                if let Some(s) = self.session.as_mut() {
                    match out {
                        WorkspaceOutput::JoinVoice => s.join_voice(),
                        WorkspaceOutput::LeaveVoice => s.leave_voice(),
                        WorkspaceOutput::Mute(b) => s.mute(b),
                        WorkspaceOutput::Deafen(b) => s.deafen(b),
                        WorkspaceOutput::StartShare => s.start_share(engine::screen::ShareConfig::default()),
                        WorkspaceOutput::StopShare => s.stop_share(),
                        WorkspaceOutput::SendChat(body) => s.send_chat(&body),
                    }
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

                let _ = self.workspace.sender().send(WorkspaceInput::SetSelf {
                    id: session.self_id(),
                    username: session.self_name().to_string(),
                });

                self.session = Some(session);
                self.screen = Screen::Workspace;

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
                let _ = self.login.sender().send(LoginInput::SetStatus(format!("Login failed: {e}")));
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
    /// Translate each engine `SessionEvent` into a workspace input.
    fn on_event(&mut self, event: SessionEvent) {
        let w = self.workspace.sender();
        match event {
            SessionEvent::Presence(Presence::Roster(list)) => {
                let _ = w.send(WorkspaceInput::Roster(list));
            }
            SessionEvent::Presence(Presence::Joined { user, username }) => {
                let _ = w.send(WorkspaceInput::PeerJoined { user, username });
            }
            SessionEvent::Presence(Presence::Left { user }) => {
                let _ = w.send(WorkspaceInput::PeerLeft { user });
            }
            SessionEvent::Chat(entry) => {
                let _ = w.send(WorkspaceInput::Chat(entry));
            }
            SessionEvent::ChatHistory(list) => {
                let _ = w.send(WorkspaceInput::ChatHistory(list));
            }
            SessionEvent::VoiceState(members) => {
                let _ = w.send(WorkspaceInput::VoiceRoster(members));
            }
            SessionEvent::VoiceJoined { user, username } => {
                let _ = w.send(WorkspaceInput::VoiceJoined { user, username });
            }
            SessionEvent::VoiceLeft { user } => {
                let _ = w.send(WorkspaceInput::VoiceLeft { user });
            }
            SessionEvent::ShareStarted { user } => {
                let _ = w.send(WorkspaceInput::ShareStarted { user });
            }
            SessionEvent::ShareStopped { user } => {
                let _ = w.send(WorkspaceInput::ShareStopped { user });
            }
            SessionEvent::VideoReady { peer, flow } => {
                if flow == hearth_protocol::Flow::Screen {
                    let paintable = self
                        .session
                        .as_ref()
                        .and_then(|s| s.paintable_for(peer, flow))
                        .and_then(|obj| obj.downcast::<gtk::gdk::Paintable>().ok());

                    if let Some(p) = paintable {
                        self.screens.borrow_mut().insert(peer, p);
                        let _ = w.send(WorkspaceInput::VideoReady);
                    }
                }
            }
            // The input meter UI lands in Task 10; for now the level is observed
            // only in the engine (it already feeds the gate there).
            SessionEvent::InputLevel(_) => {}
            SessionEvent::FlowState { .. } | SessionEvent::Error(_) => {}
        }
    }

    fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Login => "login",
            Screen::Connecting => "connecting",
            Screen::Workspace => "workspace",
        }
    }
}
