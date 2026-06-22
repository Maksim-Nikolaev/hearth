use gtk::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;

/// One rendered line in the members rail: either a section header or a member.
#[derive(Debug, Clone)]
pub struct MemberRowData {
    pub label: String,
    pub is_header: bool,
    pub in_voice: bool,
    /// True only for the self row when the local user is actively sharing screen.
    pub sharing: bool,
}

pub struct MemberRow {
    data: MemberRowData,
}

#[relm4::factory(pub)]
impl FactoryComponent for MemberRow {
    type Init = MemberRowData;
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Label {
            set_xalign: 0.0,
            set_label: &self.data.label,
            set_css_classes: &self.classes(),
        }
    }

    fn init_model(data: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        MemberRow { data }
    }
}

impl MemberRow {
    fn classes(&self) -> Vec<&'static str> {
        if self.data.is_header {
            vec!["section-header"]
        } else if self.data.in_voice && self.data.sharing {
            vec!["member", "in-voice", "sharing"]
        } else if self.data.in_voice {
            vec!["member", "in-voice"]
        } else {
            vec!["member"]
        }
    }
}

/// The right-hand members rail, grouped In Voice / Online.
pub struct Members {
    rows: FactoryVecDeque<MemberRow>,
}

#[derive(Debug)]
pub enum MembersInput {
    SetRows(Vec<MemberRowData>),
}

#[relm4::component(pub)]
impl SimpleComponent for Members {
    type Init = ();
    type Input = MembersInput;
    type Output = ();

    view! {
        gtk::ScrolledWindow {
            set_hscrollbar_policy: gtk::PolicyType::Never,
            #[local_ref]
            rows_box -> gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 1,
            }
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let rows = FactoryVecDeque::builder()
            .launch(gtk::Box::new(gtk::Orientation::Vertical, 1))
            .detach();

        let model = Members { rows };
        let rows_box = model.rows.widget();
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            MembersInput::SetRows(data) => {
                let mut guard = self.rows.guard();
                guard.clear();
                for d in data {
                    guard.push_back(d);
                }
            }
        }
    }
}
