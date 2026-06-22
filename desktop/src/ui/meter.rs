use gtk::prelude::*;
use relm4::prelude::*;

/// A horizontal level bar driven by dBFS values (roughly -60..0).
/// Maps the dBFS input linearly to the 0.0..1.0 range of `gtk::LevelBar`.
pub struct Meter {
    level: f64,
}

#[derive(Debug)]
pub enum MeterInput {
    /// Set the current level in dBFS (e.g. -60.0 = silence, 0.0 = full scale).
    SetLevel(f32),
}

#[relm4::component(pub)]
impl SimpleComponent for Meter {
    type Init = ();
    type Input = MeterInput;
    type Output = ();

    view! {
        gtk::LevelBar {
            set_min_value: 0.0,
            set_max_value: 1.0,
            set_mode: gtk::LevelBarMode::Continuous,
            set_width_request: 200,
            set_height_request: 12,
            #[watch]
            set_value: model.level,
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Meter { level: 0.0 };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            MeterInput::SetLevel(db) => {
                // Map -60..0 dBFS → 0.0..1.0, clamp outside that range.
                let normalized = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                self.level = normalized as f64;
            }
        }
    }
}
