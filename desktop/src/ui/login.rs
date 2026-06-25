use gtk::prelude::*;
use relm4::prelude::*;

/// The login form. Collects credentials and emits them to the parent, which owns
/// the session lifecycle.
pub struct LoginForm {
    status: String,
}

#[derive(Debug)]
pub enum LoginInput {
    SetStatus(String),
}

#[derive(Debug)]
pub enum LoginOutput {
    Submit { username: String, password: String },
}

#[relm4::component(pub)]
impl SimpleComponent for LoginForm {
    type Init = ();
    type Input = LoginInput;
    type Output = LoginOutput;

    view! {
        gtk::Box {
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
                // Enter from either field submits, matching the Log in button.
                connect_activate[sender, password_entry] => move |entry| {
                    let _ = sender.output(LoginOutput::Submit {
                        username: entry.text().to_string(),
                        password: password_entry.text().to_string(),
                    });
                },
            },

            #[name = "password_entry"]
            gtk::Entry {
                set_placeholder_text: Some("password"),
                set_visibility: false,
                set_width_request: 240,
                connect_activate[sender, username_entry] => move |entry| {
                    let _ = sender.output(LoginOutput::Submit {
                        username: username_entry.text().to_string(),
                        password: entry.text().to_string(),
                    });
                },
            },

            gtk::Button {
                set_label: "Log in",
                add_css_class: "suggested-action",
                connect_clicked[sender, username_entry, password_entry] => move |_| {
                    let _ = sender.output(LoginOutput::Submit {
                        username: username_entry.text().to_string(),
                        password: password_entry.text().to_string(),
                    });
                },
            },

            gtk::Label {
                #[watch]
                set_label: &model.status,
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = LoginForm { status: String::new() };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            LoginInput::SetStatus(s) => self.status = s,
        }
    }
}
