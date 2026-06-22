/// Install the dark, Discord-like CSS on the default display. Safe to call once
/// at startup after GTK is initialized.
pub fn load() {
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
";
