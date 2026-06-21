use crate::config::Config;
use engine::flow::VideoSink;
use engine::session::{Connection, Session, SessionEvent};
use gtk::prelude::*;
use hearth_protocol::ServerMessage;
use relm4::prelude::*;

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Login,
    Connecting,
    Room,
}

pub struct AppModel {
    config: Config,
    screen: Screen,
    status: String,
    session: Option<Session>,
}

#[derive(Debug)]
pub enum AppMsg {
    Login { username: String, password: String },
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
            set_title: Some("Hearth"),
            set_default_width: 960,
            set_default_height: 640,

            gtk::Stack {
                set_margin_all: 24,
                #[watch]
                set_visible_child_name: model.screen_name(),

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
                    gtk::Label {
                        set_label: "Room: main",
                        add_css_class: "title-2",
                    },
                    gtk::Label {
                        #[watch]
                        set_label: &model.status,
                    },
                },
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

        let model = AppModel { config, screen, status: String::new(), session: None };
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
            Cmd::Event(e) => {
                self.status = format!("{e:?}");
            }
        }
    }
}

impl AppModel {
    fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Login => "login",
            Screen::Connecting => "connecting",
            Screen::Room => "room",
        }
    }
}
