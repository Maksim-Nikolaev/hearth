use gtk::prelude::*;
use relm4::prelude::*;
use uuid::Uuid;

/// The center stage: a `gtk::Picture` showing the selected screenshare and a
/// "Watching" switcher over the active sharers. The picture is owned by the
/// parent (so the paintable, which is not `Send`, never rides a message) and
/// passed in; this component only renders the switcher buttons.
pub struct Stage {
    tabs_box: gtk::Box,
}

#[derive(Debug)]
pub enum StageInput {
    SetSharers(Vec<(Uuid, String)>),
}

#[derive(Debug)]
pub enum StageOutput {
    Select(Uuid),
}

#[relm4::component(pub)]
impl SimpleComponent for Stage {
    type Init = gtk::Picture;
    type Input = StageInput;
    type Output = StageOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 6,
            set_margin_all: 8,

            gtk::Frame {
                add_css_class: "stage-frame",
                set_vexpand: true,
                #[local_ref]
                picture -> gtk::Picture {
                    set_vexpand: true,
                    set_hexpand: true,
                },
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 6,
                add_css_class: "watching-bar",

                gtk::Label { set_label: "Watching:" },

                #[local_ref]
                tabs_box -> gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,
                },
            },
        }
    }

    fn init(
        picture: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);

        let model = Stage { tabs_box: tabs_box.clone() };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            StageInput::SetSharers(list) => {
                while let Some(child) = self.tabs_box.first_child() {
                    self.tabs_box.remove(&child);
                }

                for (id, label) in list {
                    let button = gtk::Button::with_label(&label);
                    let out = sender.output_sender().clone();
                    button.connect_clicked(move |_| {
                        let _ = out.send(StageOutput::Select(id));
                    });
                    self.tabs_box.append(&button);
                }
            }
        }
    }
}
