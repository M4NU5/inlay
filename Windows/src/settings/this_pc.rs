//! The This PC page: the server connection, this PC's settings, and the version.

use super::layout::{Builder, Control, Row};
use super::{
    CLIPBOARD, CONNECT, Context, ENDPOINT, HOTKEY, LAUNCH, MUTE, NAME, TOKEN, server_line,
};
use crate::app::Server;
use crate::{config, hotkey, theme};

pub fn build(page: &mut Builder, context: &Context) {
    page.title("This PC");

    page.section("Connection");
    let (dot, status) = server_line(&context.server);
    page.card(vec![
        Row::new("Server address").control(ENDPOINT, Control::Edit),
        Row::new("Access token").control(TOKEN, Control::Edit),
        Row::new("Device name").control(NAME, Control::Edit),
        Row::new(status)
            .dot(dot)
            .value(&context.settings.config.endpoint)
            .control(CONNECT, Control::button("Connect")),
        Row::caption(
            "Paste the server's admin token once (admin-token in its data folder, or its --token-file). This PC then keeps its own revocable credential.",
        ),
    ]);
    match (&context.message, &context.server) {
        (Some(message), _) => page.note(
            &message.text,
            if message.error {
                theme::ORANGE
            } else {
                theme::MUTED
            },
        ),
        (None, Server::Unavailable(message)) => page.note(message, theme::MUTED),
        _ => {}
    }
    page.gap(32);

    page.section("This PC");
    let key = hotkey::key(&context.settings.config.hotkey);
    page.card(vec![
        Row::new("Dictation key").control(HOTKEY, Control::choice(key.label, false)),
        Row::new("Mute system audio while recording").control(
            MUTE,
            Control::Switch {
                on: context.settings.config.mute_output_while_recording,
            },
        ),
        Row::new("Never leave transcripts on the clipboard").control(
            CLIPBOARD,
            Control::Switch {
                on: context.settings.config.preserve_clipboard,
            },
        ),
        Row::new("Start Inlay when I sign in")
            .control(LAUNCH, Control::Switch { on: context.launch }),
    ]);
    page.gap(32);

    let folder = config::data_dir().display().to_string();
    page.card(vec![
        Row::new("Inlay for Windows")
            .strong()
            .value(env!("CARGO_PKG_VERSION")),
        Row::new("Settings folder").value(&folder).path(),
        Row::caption("Quitting this app leaves your server running."),
    ]);
}
