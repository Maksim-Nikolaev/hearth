use gtk::prelude::*;
use relm4::prelude::*;

/// The login form. Credentials live on the main form; the server address lives
/// behind a top-right Settings button (a small dialog), since it changes rarely.
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
    /// Initial server (host:port) to pre-fill the connection dialog.
    type Init = String;
    type Input = LoginInput;
    type Output = LoginOutput;

    view! {
        gtk::Overlay {
            // The centered login form.
            #[wrap(Some)]
            set_child = &gtk::Box {
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

                // Build identity, so a stale binary (or a Linux↔Windows version
                // mismatch) is obvious before a confusing call.
                gtk::Label {
                    set_label: concat!("build ", env!("HEARTH_GIT_SHA")),
                    add_css_class: "dim-label",
                },
            },

            // Top-right gear → the connection (server) dialog.
            add_overlay = &gtk::Button {
                set_halign: gtk::Align::End,
                set_valign: gtk::Align::Start,
                set_margin_top: 12,
                set_margin_end: 12,
                set_icon_name: "emblem-system-symbolic",
                set_tooltip_text: Some("Connection settings"),
                connect_clicked[conn_dialog] => move |btn| {
                    if let Some(win) = btn.root().and_downcast::<gtk::Window>() {
                        conn_dialog.set_transient_for(Some(&win));
                    }
                    conn_dialog.present();
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // ── Connection dialog (server address) ────────────────────────────────
        let conn_dialog = gtk::Window::builder()
            .title("Connection")
            .modal(true)
            .hide_on_close(true)
            .default_width(320)
            .build();

        let dialog_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(16)
            .margin_bottom(16)
            .margin_start(16)
            .margin_end(16)
            .build();

        let server_entry = gtk::Entry::builder()
            .placeholder_text("server (host:port)")
            .text(&init)
            .build();
        dialog_box.append(&server_entry);

        // Applied indicator: appears on Save, clears on edit.
        let saved_label = gtk::Label::new(None);
        saved_label.set_xalign(0.0);
        saved_label.add_css_class("dim-label");
        dialog_box.append(&saved_label);

        let button_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .build();
        let cancel_btn = gtk::Button::with_label("Cancel");
        let save_btn = gtk::Button::with_label("Save");
        save_btn.add_css_class("suggested-action");
        button_row.append(&cancel_btn);
        button_row.append(&save_btn);
        dialog_box.append(&button_row);

        conn_dialog.set_child(Some(&dialog_box));

        // Save persists but keeps the dialog open, and flags that it applied.
        {
            let entry = server_entry.clone();
            let label = saved_label.clone();
            save_btn.connect_clicked(move |_| {
                crate::config::save_server(&entry.text());
                label.set_text("Saved ✓ — used on next login");
            });
        }

        // Editing clears the indicator (the shown text no longer matches saved).
        {
            let label = saved_label.clone();
            server_entry.connect_changed(move |_| label.set_text(""));
        }

        // Cancel closes without saving; reset the field to the saved value so a
        // reopen doesn't show discarded edits.
        {
            let dialog = conn_dialog.clone();
            let entry = server_entry.clone();
            cancel_btn.connect_clicked(move |_| {
                entry.set_text(&crate::config::initial_server());
                dialog.close();
            });
        }

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
