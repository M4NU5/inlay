//! The Microphone page: which input Inlay records from, and a test whose
//! transcript shows underneath.

use super::layout::{Builder, Control, Row};
use super::{Context, MICROPHONE, TEST, test_button};
use crate::theme;

pub fn build(page: &mut Builder, context: &Context) {
    page.title("Microphone");
    let default = context
        .default_microphone
        .clone()
        .unwrap_or_else(|| "No microphone".into());
    let (choice, next, note) = match &context.settings.config.microphone {
        None => (
            "System default".to_string(),
            default,
            "Inlay records from the Windows default input.".to_string(),
        ),
        Some(name) if context.microphones.contains(name) => (
            name.clone(),
            name.clone(),
            "Inlay always records from this microphone.".to_string(),
        ),
        Some(name) => (
            format!("{name} (disconnected)"),
            default,
            format!("{name} is disconnected, so the Windows default input is used."),
        ),
    };
    page.card(vec![
        Row::new("Choose input").control(MICROPHONE, Control::choice(&choice, true)),
        Row::new("Next dictation").value(&next),
    ]);
    page.note(&note, theme::MUTED);
    page.gap(32);
    page.actions(vec![(TEST, test_button(context))]);
    test_result(page, context);
}

/// What the test heard, boxed under the button, while it runs and after.
fn test_result(page: &mut Builder, context: &Context) {
    if context.test_busy {
        let status = if context.testing {
            "Listening… Speak, then select Finish dictation."
        } else {
            "Transcribing…"
        };
        page.gap(16);
        page.text_card("Test result", "", status, theme::MUTED, "");
        return;
    }
    let Some(last) = context.last.as_ref().filter(|last| last.test) else {
        return;
    };
    let seconds = format!("{:.1} s audio", last.seconds);
    page.gap(16);
    if last.text.is_empty() {
        page.text_card(
            "Test result",
            &seconds,
            "No speech detected. Check the microphone and try again.",
            theme::MUTED,
            "",
        );
    } else {
        page.text_card(
            "Test result",
            &seconds,
            &last.text,
            theme::INK,
            "Nothing was pasted.",
        );
    }
}
