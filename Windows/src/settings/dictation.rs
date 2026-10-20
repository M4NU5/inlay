//! The Dictation page: how to dictate, whether the server is online, and the
//! last dictation with Copy, as on the Mac's home page.

use super::layout::{Builder, Control, Icon, Item};
use super::{CHECK, COPY, Context, GEAR, TEST, server_line, test_button};
use crate::app::Server;
use crate::hotkey;
use crate::theme::{self, Font, px};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
};

const LINE: u32 = DT_SINGLELINE | DT_VCENTER;

pub fn build(page: &mut Builder, context: &Context) {
    hero(page, context);
    page.gap(26);

    let (dot, status) = server_line(&context.server);
    let check = Control::button("Check connection").enabled(context.server != Server::Checking);
    page.status(
        dot,
        status,
        &context.settings.config.endpoint,
        Some((CHECK, check)),
    );
    if let Server::Unavailable(message) = &context.server {
        page.note(message, theme::MUTED);
    }
    page.gap(26);

    transcript(page, context);
    page.gap(24);

    page.actions(vec![
        (TEST, test_button(context)),
        (GEAR, Control::icon(Icon::Gear, "This PC settings")),
    ]);
}

/// The dictation key as a key cap, and how to use it.
fn hero(page: &mut Builder, context: &Context) {
    let key = hotkey::key(&context.settings.config.hotkey);
    let area = page.band(px(76));
    let width = (theme::text_width(key.label, Font::Strong) / 2 + px(36)).max(px(68));
    let cap = RECT {
        left: area.left,
        top: area.top,
        right: area.left + width,
        bottom: area.top + px(68),
    };
    page.push(Item::KeyCap {
        area: cap,
        label: key.label.into(),
        pressed: context.recording && !context.testing,
    });
    page.text(
        "Hold to dictate.",
        RECT {
            left: cap.right + px(24),
            top: cap.top,
            right: area.right,
            bottom: cap.bottom + px(4),
        },
        Font::Hero,
        theme::INK,
        DT_LEFT | LINE | DT_END_ELLIPSIS,
    );
}

/// The current or last dictation between hairlines, with what became of it.
fn transcript(page: &mut Builder, context: &Context) {
    let busy = context.recording || context.processing;
    let text = context.last.as_ref().map_or("", |last| last.text.as_str());
    let header = page.band(px(32));
    page.text(
        if busy {
            "Current dictation"
        } else {
            "Last dictation"
        },
        header,
        Font::Strong,
        theme::INK,
        DT_LEFT | LINE,
    );
    let copy = Control::button("Copy")
        .with_icon(Icon::Copy)
        .enabled(!busy && !text.is_empty());
    page.place_right(
        COPY,
        copy,
        header.right,
        (header.top + header.bottom) / 2,
        theme::CANVAS,
    );
    page.gap(10);
    page.rule();

    let width = page.width();
    let wrapped = DT_LEFT | DT_WORDBREAK | DT_NOPREFIX;
    if busy {
        let (title, detail) = if context.recording {
            let key = hotkey::key(&context.settings.config.hotkey).label;
            (
                "Listening…",
                if context.testing {
                    "Speak, then select Finish dictation. Nothing will be pasted.".to_string()
                } else {
                    format!("Speak, then release {key}.")
                },
            )
        } else {
            ("Transcribing…", "Your words are on the way.".to_string())
        };
        let area = page.band(px(170));
        let middle = (area.top + area.bottom) / 2;
        page.text(
            title,
            RECT {
                top: middle - px(22),
                bottom: middle,
                ..area
            },
            Font::Strong,
            theme::INK,
            DT_LEFT | LINE,
        );
        page.text(
            &detail,
            RECT {
                top: middle + px(2),
                bottom: middle + px(22),
                ..area
            },
            Font::Small,
            theme::MUTED,
            DT_LEFT | LINE | DT_END_ELLIPSIS,
        );
    } else {
        let (shown, colour) = if text.is_empty() {
            ("Your next thought will appear here.", theme::MUTED)
        } else {
            (text, theme::INK)
        };
        let height = theme::text_height(shown, Font::Body, width, wrapped);
        let area = page.band((height + px(28)).max(px(170)));
        page.text(
            shown,
            RECT {
                top: area.top + px(14),
                ..area
            },
            Font::Body,
            colour,
            wrapped,
        );
    }
    page.rule();

    let footer = page.band(px(36));
    let (outcome, colour) = match (&context.error, &context.last) {
        (Some(error), _) if !busy => (error.as_str(), theme::ORANGE),
        (_, Some(last)) if !busy => (last.outcome.as_str(), theme::MUTED),
        _ => ("", theme::MUTED),
    };
    let seconds = context
        .last
        .as_ref()
        .filter(|_| !busy)
        .map(|last| format!("{:.1} s audio", last.seconds));
    let audio_width = seconds.as_deref().map_or(0, |seconds| {
        theme::text_width(seconds, Font::Small) + px(16)
    });
    page.text(
        outcome,
        RECT {
            right: footer.right - audio_width,
            ..footer
        },
        Font::Small,
        colour,
        DT_LEFT | LINE | DT_END_ELLIPSIS,
    );
    if let Some(seconds) = seconds {
        page.text(&seconds, footer, Font::Small, theme::INK, DT_RIGHT | LINE);
    }
}
