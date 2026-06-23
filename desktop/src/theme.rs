/// Install the dark, Discord-like CSS on the default display. Safe to call once
/// at startup after GTK is initialized.
pub fn load() {
    // The CSS below only colours our own widget classes; the base GTK widgets
    // (buttons, entries, scrollbars, header bars, popovers) follow the active
    // Adwaita variant. Without this, that variant defaults to *light* on
    // Windows (where no desktop environment sets a dark preference), so the
    // chrome renders light against our dark panels. Request the dark variant so
    // the whole UI is consistent. Hearth is a dark-themed app by design.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }

    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);

    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

const CSS: &str = "
window { background-color: #2b2d31; color: #dbdee1; }

.rail { background-color: #1e1f22; }

.section-header {
    color: #949ba4;
    font-size: 11px;
    font-weight: bold;
    padding: 8px 8px 2px 8px;
}

.member, .channel {
    padding: 4px 8px;
    border-radius: 4px;
    color: #c7cad1;
}
.member.in-voice { color: #f2f3f5; }
.channel-active { background-color: #404249; color: #ffffff; }

.self-name { font-weight: bold; color: #f2f3f5; }

.stage-frame {
    background-color: #000000;
    border-radius: 8px;
}

.watching-bar { padding: 4px; }

.chat-line { padding: 1px 4px; }
.chat-author { font-weight: bold; color: #f2f3f5; }

button.suggested-action, .accent { background-color: #5865f2; color: #ffffff; }

/* Active screen-share indicator: red background on the Share button and the
   self row in the members rail. */
button.sharing { background-color: #ed4245; color: #ffffff; }
.member.sharing { color: #ed4245; font-weight: bold; }

entry {
    background-color: #383a40;
    color: #dbdee1;
    border-radius: 6px;
}

/* Screen-share source cards in the picker grid. */
.source-card {
    background-color: #313338;
    border-radius: 8px;
    padding: 6px;
    border: 2px solid transparent;
}

.source-card.selected {
    border-color: #5865f2;
    background-color: #3c3f45;
}

.source-card-title {
    color: #dbdee1;
    font-size: 12px;
}
";
