use crate::config::{ActivationKind, Config, NsLevel};
use crate::ui::login::{LoginForm, LoginInput, LoginOutput};
use crate::ui::settings::{SettingsInput, SettingsOutput, SettingsWindow};
use crate::ui::workspace::{Screens, Workspace, WorkspaceInput, WorkspaceOutput};
use engine::audio::devices::list_devices;
use engine::audio::dsp::{DspConfig, NsLevel as EngineNsLevel};
use engine::audio::gate::ActivationMode;
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
    settings_window: Controller<SettingsWindow>,
}

#[derive(Debug)]
pub enum AppMsg {
    Login { username: String, password: String },
    Workspace(WorkspaceOutput),
    Settings(SettingsOutput),
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

        let settings_window = SettingsWindow::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::Settings);

        let model = AppModel {
            config,
            title,
            screen,
            session: None,
            screens,
            login,
            workspace,
            settings_window,
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
                match out {
                    WorkspaceOutput::OpenSettings => {
                        let saved = self.config.load_settings();
                        let _ = self.settings_window.sender().send(
                            SettingsInput::SetDevices(list_devices()),
                        );
                        let _ = self.settings_window.sender().send(
                            SettingsInput::SetSettings(saved),
                        );
                        self.settings_window.widget().present();
                    }
                    out => {
                        if let Some(s) = self.session.as_mut() {
                            match out {
                                WorkspaceOutput::JoinVoice => s.join_voice(),
                                WorkspaceOutput::LeaveVoice => s.leave_voice(),
                                WorkspaceOutput::Mute(b) => s.mute(b),
                                WorkspaceOutput::Deafen(b) => s.deafen(b),
                                WorkspaceOutput::StartShare => s.start_share(engine::screen::ShareConfig::default()),
                                WorkspaceOutput::StopShare => s.stop_share(),
                                WorkspaceOutput::SendChat(body) => s.send_chat(&body),
                                WorkspaceOutput::OpenSettings => unreachable!(),
                            }
                        }
                    }
                }
            }
            AppMsg::Settings(out) => {
                self.apply_settings_output(out);
            }
        }
    }

    fn update_cmd(&mut self, msg: Self::CommandOutput, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            Cmd::Opened(conn) => {
                self.config.save_token(conn.token());

                let (mut session, mut inbound, mut events) = Session::start(conn, VideoSink::Paintable);
                session.join("main");

                let _ = self.workspace.sender().send(WorkspaceInput::SetSelf {
                    id: session.self_id(),
                    username: session.self_name().to_string(),
                });

                // Apply persisted audio settings so saved prefs take effect on connect.
                let saved = self.config.load_settings();
                Self::apply_settings_to_session(&mut session, &saved);

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
            SessionEvent::InputLevel(db) => {
                let _ = self.settings_window.sender().send(SettingsInput::SetLevel(db));
            }
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

    /// Persist a settings change and, if connected, apply it to the live session.
    fn apply_settings_output(&mut self, out: SettingsOutput) {
        let mut settings = self.config.load_settings();

        match out {
            SettingsOutput::InputDevice(id) => settings.input_device = id,
            SettingsOutput::OutputDevice(id) => settings.output_device = id,
            SettingsOutput::InputVolume(v) => settings.input_volume = v,
            SettingsOutput::OutputVolume(v) => settings.output_volume = v,
            SettingsOutput::NoiseSuppression(ns) => settings.noise_suppression = ns,
            SettingsOutput::EchoCancellation(b) => settings.echo_cancellation = b,
            SettingsOutput::Agc(b) => settings.agc = b,
            SettingsOutput::Vad(b) => settings.vad = b,
            SettingsOutput::InputSensitivity(db) => settings.input_sensitivity = db,
            SettingsOutput::Activation(a) => settings.activation = a,
            SettingsOutput::PttKey(k) => settings.ptt_key = k,
            SettingsOutput::MicTest(on) => {
                if let Some(s) = self.session.as_mut() {
                    if on { s.start_mic_test() } else { s.stop_mic_test() }
                }
                return; // mic-test is not persisted
            }
        }

        self.config.save_settings(&settings);

        if let Some(s) = self.session.as_mut() {
            Self::apply_settings_to_session(s, &settings);
        }
    }

    /// Push all audio-relevant settings to a live `Session`.
    fn apply_settings_to_session(session: &mut Session, s: &crate::config::Settings) {
        session.set_dsp(DspConfig {
            echo_cancel: s.echo_cancellation,
            noise_suppression: match s.noise_suppression {
                NsLevel::Off => EngineNsLevel::Off,
                NsLevel::Low => EngineNsLevel::Low,
                NsLevel::Moderate => EngineNsLevel::Moderate,
                NsLevel::High => EngineNsLevel::High,
            },
            agc: s.agc,
            vad: s.vad,
            high_pass: true,
        });

        session.set_activation(match s.activation {
            ActivationKind::Voice => ActivationMode::Voice { threshold: s.input_sensitivity },
            ActivationKind::PushToTalk => ActivationMode::PushToTalk,
            ActivationKind::AlwaysOn => ActivationMode::AlwaysOn,
        });

        session.set_input_device(s.input_device.clone());
        session.set_output_device(s.output_device.clone());
        session.set_ptt_key(s.ptt_key.clone());
    }
}
