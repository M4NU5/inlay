//! The Microphone page: which input Inlay records from, and a test.

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
}
