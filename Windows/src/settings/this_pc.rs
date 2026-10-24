//! The This PC page: the server connection, this PC's settings, and the version.

use super::layout::{Builder, Control, Row};
use super::{
    CLIPBOARD, CONNECT, Context, ENDPOINT, HOTKEY, LAUNCH, MIC_SETTINGS, MUTE, NAME,
    SHORTCUT_CHECK, TOKEN, TRIGGER, server_line,
};
use crate::app::Server;
use crate::{config, hotkey, privacy, theme};

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
    let trigger = hotkey::Trigger::from_id(&context.settings.config.activation_mode);
    let mut rows = vec![
        Row::new("Dictation key").control(HOTKEY, Control::choice(key.label, false)),
        Row::new("Trigger").control(TRIGGER, Control::choice(trigger.label(), false)),
    ];
    if trigger == hotkey::Trigger::Tap {
        rows.push(Row::caption(
            "Recording continues until you tap again or reach 3 minutes.",
        ));
    }
    let (check, button) = match (context.checking, trigger) {
        (false, _) => ("Shortcut check", "Check shortcut"),
        (true, hotkey::Trigger::Hold) => ("Hold the key for a second", "Stop checking"),
        (true, hotkey::Trigger::DoubleTap) => ("Double tap the key", "Stop checking"),
        (true, hotkey::Trigger::Tap) => ("Tap the key", "Stop checking"),
    };
    rows.extend([
        Row::new(check).control(
            SHORTCUT_CHECK,
            Control::button(button).enabled(context.checking || !context.recording),
        ),
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
    page.card(rows);
    if context.checking || !context.check_lines.is_empty() {
        page.gap(12);
        page.text_card(
            "Shortcut diagnostics",
            "",
            &context.check_lines.join("\n"),
            theme::MUTED,
            "",
        );
    }
    page.gap(32);

    page.section("Permissions");
    let blocked = privacy::microphone_blocked();
    let (dot, access) = match blocked {
        None => (theme::GREEN, "Allowed"),
        Some(_) => (theme::ORANGE, "Blocked"),
    };
    page.card(vec![
        Row::new("Microphone")
            .dot(dot)
            .detail("Capture audio while dictating.")
            .value(access)
            .control(MIC_SETTINGS, Control::button("Open settings")),
    ]);
    if let Some(switch) = blocked {
        page.note(
            &format!(
                "Windows is keeping Inlay from the microphone, so it can't hear you. In Settings, turn on “{switch}”."
            ),
            theme::ORANGE,
        );
    }
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
