use gtk::prelude::*;
use hearth_protocol::ChatEntry;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;

pub struct MessageRow {
    author: String,
    body: String,
}

#[relm4::factory(pub)]
impl FactoryComponent for MessageRow {
    type Init = ChatEntry;
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 6,
            add_css_class: "chat-line",

            gtk::Label {
                set_label: &self.author,
                add_css_class: "chat-author",
                set_xalign: 0.0,
                set_valign: gtk::Align::Start,
            },
            gtk::Label {
                set_label: &self.body,
                set_xalign: 0.0,
                set_wrap: true,
                set_hexpand: true,
            },
        }
    }

    fn init_model(entry: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        MessageRow { author: entry.username, body: entry.body }
    }
}

/// The chat panel: a scrolling message list plus an input entry.
pub struct Chat {
    rows: FactoryVecDeque<MessageRow>,
}

#[derive(Debug)]
pub enum ChatInput {
    Reset(Vec<ChatEntry>),
    Append(ChatEntry),
}

#[derive(Debug)]
pub enum ChatOutput {
    Send(String),
}

#[relm4::component(pub)]
impl SimpleComponent for Chat {
    type Init = ();
    type Input = ChatInput;
    type Output = ChatOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 4,
            set_margin_all: 8,

            #[name = "scroller"]
            gtk::ScrolledWindow {
                set_vexpand: true,
                set_hscrollbar_policy: gtk::PolicyType::Never,
                #[local_ref]
                rows_box -> gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 2,
                    set_valign: gtk::Align::End,
                },
            },

            gtk::Entry {
                set_placeholder_text: Some("Message # general"),
                connect_activate[sender] => move |entry| {
                    let body = entry.text().to_string();
                    if !body.is_empty() {
                        let _ = sender.output(ChatOutput::Send(body));
                        entry.set_text("");
                    }
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let rows = FactoryVecDeque::builder()
            .launch(gtk::Box::new(gtk::Orientation::Vertical, 2))
            .detach();

        let model = Chat { rows };
        let rows_box = model.rows.widget();
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            ChatInput::Reset(list) => {
                let mut guard = self.rows.guard();
                guard.clear();
                for e in list {
                    guard.push_back(e);
                }
            }
            ChatInput::Append(entry) => {
                self.rows.guard().push_back(entry);
            }
        }
    }
}
