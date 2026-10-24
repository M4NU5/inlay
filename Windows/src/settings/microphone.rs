//! The Microphone page, as on the Mac: which input Inlay records from, the
//! saved priority lists behind Automatic, and a test whose transcript shows
//! underneath. Changes save as they are made.
//!
//! A priority list's buttons come and go with its microphones. Each has a
//! `Slot` with its own control id, which the window makes when the layout
//! first places it.

use super::layout::{Builder, Control, Icon, Item, Row};
use super::{
    Context, LIST_NAME, LIST_OPTIONS, MICROPHONE, Message, NAME_CANCEL, NAME_SAVE, NEW_LIST,
    PICK_LIST, PICK_MICROPHONE, PICK_PROFILE, PROFILE, SLOTS, TEST, USE_LIST, say, set_text,
    test_button,
};
use crate::app;
use crate::menu::Entry;
use crate::microphones::{Input, Microphones, Reason, Resolution, Selection};
use crate::theme::{self, Font, px};
use crate::win::wide;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetDlgItem, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW,
};

const LINE: u32 = DT_SINGLELINE | DT_VCENTER;
/// Space between the card's edge and its rows, in logical pixels.
const INSET: i32 = 16;
const TOOLBAR: i32 = 54;
const ROW: i32 = 52;
const HEADING: i32 = 34;
const EMPTY: i32 = 64;
const NAMING: i32 = 92;

/// A priority list button, by the microphone it belongs to.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Slot {
    Up(String),
    Down(String),
    Remove(String),
    Add(String),
}

/// A list being named: a new one, or `list` being renamed.
struct Naming {
    list: Option<String>,
    text: String,
    error: Option<String>,
}

#[derive(Default)]
struct Page {
    naming: Option<Naming>,
    /// Behind the input popup's entries, in order.
    choices: Vec<Selection>,
    /// Behind the list popup's entries, by list id.
    lists: Vec<String>,
    slots: HashMap<Slot, i32>,
    by_id: Vec<Slot>,
}

static PAGE: LazyLock<Mutex<Page>> = LazyLock::new(Mutex::default);

fn page() -> MutexGuard<'static, Page> {
    PAGE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The control id for `slot`, made up the first time it is asked for.
fn id(page: &mut Page, slot: Slot) -> i32 {
    if let Some(id) = page.slots.get(&slot) {
        return *id;
    }
    let id = SLOTS + page.by_id.len() as i32;
    page.by_id.push(slot.clone());
    page.slots.insert(slot, id);
    id
}

fn saved() -> Microphones {
    app::shared().settings().config.microphones
}

/// Applies an edit to the saved preferences, and saves them if it changed
/// anything.
fn change(edit: impl FnOnce(&mut Microphones) -> Result<(), String>) -> Result<(), String> {
    let current = saved();
    let mut next = current.clone();
    edit(&mut next)?;
    let next = next.normalized();
    if next == current {
        return Ok(());
    }
    app::shared().change_config(|config| config.microphones = next)?;
    log!("settings: saved a microphone change on this PC");
    Ok(())
}

/// Shows a failure to save under the list, as the Mac does.
fn report(result: Result<(), String>) {
    say(result.err().map(|text| Message { text, error: true }));
}

/// Inputs came or went: keep the names saved lists show up to date.
pub fn devices_changed(inputs: &[Input]) {
    if let Err(error) = change(|microphones| {
        microphones.rename_connected(inputs);
        Ok(())
    }) {
        log!("settings: could not save microphone names: {error}");
    }
}

/// The popup's entries for the input choice or the list choice.
pub fn choices(id: i32, context: &Context) -> Vec<Entry> {
    let microphones = &context.settings.config.microphones;
    let mut page = page();
    let item = |id: usize, label: String, checked: bool| Entry::Item {
        id,
        label,
        enabled: true,
        checked,
    };
    if id == PROFILE {
        page.lists = microphones
            .profiles
            .iter()
            .map(|each| each.id.clone())
            .collect();
        return microphones
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let checked = profile.id == microphones.active().id;
                item(PICK_PROFILE + index, profile.name.clone(), checked)
            })
            .collect();
    }
    if id == LIST_OPTIONS {
        return vec![
            item(PICK_LIST, "Rename list…".into(), false),
            Entry::Item {
                id: PICK_LIST + 1,
                label: "Delete list…".into(),
                enabled: microphones.profiles.len() > 1,
                checked: false,
            },
        ];
    }
    let mut choices = vec![Selection::Automatic, Selection::SystemDefault];
    choices.extend(context.inputs.iter().map(|device| Selection::Fixed {
        device: device.clone(),
    }));
    if let Selection::Fixed { device } = &microphones.selection
        && !connected(context, device)
    {
        choices.push(microphones.selection.clone());
    }
    let mut entries = Vec::new();
    for (index, choice) in choices.iter().enumerate() {
        let checked = match (choice, &microphones.selection) {
            (Selection::Fixed { device }, Selection::Fixed { device: saved }) => {
                device.uid == saved.uid
            }
            (choice, saved) => choice == saved,
        };
        entries.push(item(
            PICK_MICROPHONE + index,
            label(choice, context),
            checked,
        ));
        if index == 1 && choices.len() > 2 {
            entries.push(Entry::Separator);
        }
    }
    page.choices = choices;
    entries
}

fn label(selection: &Selection, context: &Context) -> String {
    match selection {
        Selection::Automatic => "Automatic · priority list".into(),
        Selection::SystemDefault => "System default".into(),
        Selection::Fixed { device } if connected(context, device) => device.name.clone(),
        Selection::Fixed { device } => format!("{} (disconnected)", device.name),
    }
}

fn connected(context: &Context, device: &Input) -> bool {
    context.inputs.iter().any(|input| input.uid == device.uid)
}

/// Applies a choice from one of the page's popups. Returns the control the
/// keyboard goes back to.
pub fn pick(window: HWND, id: usize) -> i32 {
    if id >= PICK_LIST {
        let active = saved().active().clone();
        if id == PICK_LIST {
            start_naming(window, Some((active.id, active.name)));
            return LIST_NAME;
        }
        delete_list(window, &active.id, &active.name);
        return LIST_OPTIONS;
    }
    if id >= PICK_PROFILE {
        let list = page().lists.get(id - PICK_PROFILE).cloned();
        if let Some(list) = list {
            report(change(|microphones| {
                if microphones.profiles.iter().any(|each| each.id == list) {
                    microphones.active_profile_id = list;
                }
                Ok(())
            }));
        }
        return PROFILE;
    }
    let choice = page().choices.get(id - PICK_MICROPHONE).cloned();
    if let Some(choice) = choice {
        report(change(|microphones| {
            microphones.selection = choice;
            Ok(())
        }));
    }
    MICROPHONE
}

/// Use list: record from the active list again.
pub fn use_list() {
    report(change(|microphones| {
        microphones.selection = Selection::Automatic;
        Ok(())
    }));
}

/// Opens the name box for a new list, or to rename `list`.
pub fn start_naming(window: HWND, list: Option<(String, String)>) {
    let (list, text) = match list {
        Some((id, name)) => (Some(id), name),
        None => (None, String::new()),
    };
    page().naming = Some(Naming {
        list,
        text: text.clone(),
        error: None,
    });
    set_text(window, LIST_NAME, &text);
}

/// The keyboard goes to the name box once the layout has placed it.
pub fn focus_name(window: HWND) {
    if page().naming.is_some() {
        unsafe { SetFocus(GetDlgItem(window, LIST_NAME)) };
    }
}

/// The name box's text changed. Returns whether the page needs laying out.
pub fn typed(text: &str) -> bool {
    let mut page = page();
    let Some(naming) = page.naming.as_mut() else {
        return false;
    };
    let before = (naming.text.trim().is_empty(), naming.error.is_some());
    naming.text = text.to_string();
    naming.error = None;
    before != (naming.text.trim().is_empty(), false)
}

/// Create or Save: names the list, or says why it can't.
pub fn save_name() {
    let Some((list, text)) = page()
        .naming
        .as_ref()
        .map(|naming| (naming.list.clone(), naming.text.clone()))
    else {
        return;
    };
    let result = change(|microphones| match &list {
        Some(list) => microphones.rename_profile(list, &text),
        None => microphones.add_profile(&text),
    });
    let mut page = page();
    match result {
        Ok(()) => page.naming = None,
        Err(error) => {
            if let Some(naming) = page.naming.as_mut() {
                naming.error = Some(error);
            }
        }
    }
}

pub fn cancel_naming() {
    page().naming = None;
}

pub fn naming() -> bool {
    page().naming.is_some()
}

fn delete_list(window: HWND, list: &str, name: &str) {
    let text = wide(&format!(
        "Delete “{name}”?\n\nOnly this saved order is removed. Your microphones and other lists are unchanged."
    ));
    let title = wide("Inlay");
    let answer = unsafe {
        MessageBoxW(
            window,
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if answer == IDYES {
        page().naming = None;
        report(change(|microphones| {
            microphones.remove_profile(list);
            Ok(())
        }));
    }
}

/// Acts on a priority list button. Returns whether it was one.
pub fn command(id: i32, context: &Context) -> bool {
    let slot = {
        let page = page();
        match page
            .by_id
            .get(usize::try_from(id - SLOTS).unwrap_or(usize::MAX))
        {
            Some(slot) => slot.clone(),
            None => return false,
        }
    };
    report(change(|microphones| {
        match slot {
            Slot::Up(uid) => microphones.move_priority(&uid, -1),
            Slot::Down(uid) => microphones.move_priority(&uid, 1),
            Slot::Remove(uid) => microphones.remove_from_priority(&uid),
            Slot::Add(uid) => {
                if let Some(input) = context.inputs.iter().find(|input| input.uid == uid) {
                    microphones.add_to_priority(input.clone());
                }
            }
        }
        Ok(())
    }));
    true
}

pub fn build(builder: &mut Builder, context: &Context) {
    builder.title("Microphone");
    let microphones = &context.settings.config.microphones;
    let resolution = microphones.resolve(&context.inputs, context.default_input.as_ref());
    let next = resolution
        .device
        .as_ref()
        .map_or("No microphone available", |device| device.name.as_str());
    builder.card(vec![
        Row::new("Choose input").control(
            MICROPHONE,
            Control::choice(&label(&microphones.selection, context), true),
        ),
        Row::new("Next dictation").value(next),
    ]);
    builder.note(&detail(microphones, &resolution), theme::MUTED);
    builder.gap(25);

    builder.section("Input priority");
    let mut page = page();
    priority(builder, &mut page, context, microphones);
    drop(page);
    let (footer, colour) = match &context.message {
        Some(message) if message.error => (message.text.as_str(), theme::ORANGE),
        _ => (
            "Use the arrows to reorder. Disconnected microphones keep their place.",
            theme::MUTED,
        ),
    };
    builder.note(footer, colour);
    builder.gap(32);
    builder.actions(vec![(TEST, test_button(context))]);
    test_result(builder, context);
}

/// The line under the input card, in the Mac's words.
fn detail(microphones: &Microphones, resolution: &Resolution) -> String {
    match &resolution.reason {
        Reason::Fallback(requested) => format!(
            "{} is disconnected. It will be used again when it reconnects.",
            requested.name
        ),
        Reason::Unavailable => "Connect an audio input to start dictating.".into(),
        Reason::Priority => format!(
            "The first connected microphone in “{}” is used.",
            microphones.active().name
        ),
        Reason::Fixed => "This microphone is preferred whenever it is connected.".into(),
        Reason::SystemDefault if microphones.selection == Selection::SystemDefault => {
            "Follows the Windows input. Your system settings stay unchanged.".into()
        }
        Reason::SystemDefault => {
            "Using the Windows input until a preferred microphone is connected.".into()
        }
    }
}

/// The card of priority lists: which list, its microphones in order, and the
/// connected ones that aren't in it.
fn priority(builder: &mut Builder, page: &mut Page, context: &Context, microphones: &Microphones) {
    let profile = microphones.active();
    let others: Vec<&Input> = context
        .inputs
        .iter()
        .filter(|input| !profile.priority.iter().any(|each| each.uid == input.uid))
        .collect();
    let empty = profile.priority.is_empty();
    // Rows have a hairline between them, not after the last.
    let rows = |count: usize| count as i32 * px(ROW) + (count as i32 - 1).max(0);
    let mut height = px(TOOLBAR) + 1 + rows(profile.priority.len()) + px(4);
    if page.naming.is_some() {
        height += px(NAMING) + 1;
    }
    if empty {
        height += px(EMPTY);
    }
    if !others.is_empty() {
        height += px(HEADING) + rows(others.len());
    }
    let card = builder.band(height);
    builder.push(Item::Card(card));
    let (left, right) = (card.left + px(INSET), card.right - px(INSET));
    let rule = |builder: &mut Builder, y: &mut i32| {
        builder.push(Item::Rule(RECT {
            left,
            top: *y,
            right,
            bottom: *y + 1,
        }));
        *y += 1;
    };

    let mut y = card.top;
    let middle = y + px(TOOLBAR) / 2;
    let options = builder.place_right(
        LIST_OPTIONS,
        Control::icon(Icon::More, "Priority list options"),
        right + px(4),
        middle,
        theme::SURFACE,
    );
    let add = builder.place_right(
        NEW_LIST,
        Control::icon(Icon::Add, "New priority list"),
        options.left - px(4),
        middle,
        theme::SURFACE,
    );
    let automatic = microphones.selection == Selection::Automatic;
    let use_list = builder.place_right(
        USE_LIST,
        Control::button(if automatic { "In use" } else { "Use list" }).enabled(!automatic),
        add.left - px(10),
        middle,
        theme::SURFACE,
    );
    let list = Control::choice(&profile.name, true);
    let width = list.width().min(use_list.left - px(16) - (left - px(4)));
    builder.place(
        PROFILE,
        list,
        RECT {
            left: left - px(4),
            top: middle - px(15),
            right: left - px(4) + width,
            bottom: middle + px(15),
        },
        theme::SURFACE,
    );
    y += px(TOOLBAR);
    rule(builder, &mut y);

    if let Some(naming) = &page.naming {
        naming_rows(builder, naming, left, right, y);
        y += px(NAMING);
        rule(builder, &mut y);
    }

    if empty {
        let (title, hint) = if context.inputs.is_empty() {
            (
                "No microphones connected",
                "Connect an audio input to add it to this list.",
            )
        } else {
            (
                "No priorities yet",
                "Add a microphone from the connected inputs below.",
            )
        };
        let area = RECT {
            left,
            top: y + px(12),
            right,
            bottom: y + px(34),
        };
        builder.text(title, area, Font::Body, theme::INK, DT_LEFT | LINE);
        let below = RECT {
            top: area.bottom,
            bottom: area.bottom + px(18),
            ..area
        };
        builder.text(hint, below, Font::Small, theme::MUTED, DT_LEFT | LINE);
        y += px(EMPTY);
    }

    let count = profile.priority.len();
    for (position, saved) in profile.priority.iter().enumerate() {
        if position > 0 {
            rule(builder, &mut y);
        }
        let live = context.inputs.iter().find(|input| input.uid == saved.uid);
        let device = live.unwrap_or(saved);
        let middle = y + px(ROW) / 2;
        let name = &device.name;
        let remove = id(page, Slot::Remove(saved.uid.clone()));
        let remove = builder.place_right(
            remove,
            Control::icon(Icon::Remove, &format!("Remove {name} from priority list")),
            right + px(4),
            middle,
            theme::SURFACE,
        );
        let down = id(page, Slot::Down(saved.uid.clone()));
        let down = builder.place_right(
            down,
            Control::icon(Icon::Down, &format!("Move {name} down")).enabled(position + 1 < count),
            remove.left - px(2),
            middle,
            theme::SURFACE,
        );
        let up = id(page, Slot::Up(saved.uid.clone()));
        let up = builder.place_right(
            up,
            Control::icon(Icon::Up, &format!("Move {name} up")).enabled(position > 0),
            down.left - px(2),
            middle,
            theme::SURFACE,
        );
        builder.text(
            &(position + 1).to_string(),
            RECT {
                left,
                top: y,
                right: left + px(18),
                bottom: y + px(ROW),
            },
            Font::Body,
            theme::MUTED,
            DT_LEFT | LINE,
        );
        let status = if live.is_some() {
            "Connected"
        } else {
            "Disconnected"
        };
        input_row(
            builder,
            device,
            live.is_some(),
            status,
            left + px(30),
            up.left - px(12),
            y,
        );
        y += px(ROW);
    }

    if !others.is_empty() {
        let heading = RECT {
            left,
            top: y + px(12),
            right,
            bottom: y + px(HEADING) - px(2),
        };
        builder.text(
            "Connected inputs",
            heading,
            Font::Small,
            theme::MUTED,
            DT_LEFT | LINE,
        );
        y += px(HEADING);
        for (index, input) in others.into_iter().enumerate() {
            if index > 0 {
                rule(builder, &mut y);
            }
            let middle = y + px(ROW) / 2;
            let add = id(page, Slot::Add(input.uid.clone()));
            let add = builder.place_right(
                add,
                Control::icon(Icon::Add, &format!("Add {} to priority list", input.name)),
                right + px(4),
                middle,
                theme::SURFACE,
            );
            input_row(
                builder,
                input,
                true,
                "Connected",
                left + px(30),
                add.left - px(12),
                y,
            );
            y += px(ROW);
        }
    }
}

/// A microphone's icon and name from `left`, and its state ending at `right`.
fn input_row(
    builder: &mut Builder,
    input: &Input,
    available: bool,
    status: &str,
    left: i32,
    right: i32,
    top: i32,
) {
    let (icon, ink) = if available {
        (theme::ACCENT, theme::INK)
    } else {
        (theme::MUTED, theme::MUTED)
    };
    let size = px(20);
    builder.push(Item::Icon {
        icon: Icon::Microphone,
        x: left,
        y: top + (px(ROW) - size) / 2,
        size,
        colour: icon,
    });
    let status_width = theme::text_width(status, Font::Small);
    let row = RECT {
        left: left + size + px(10),
        top,
        right,
        bottom: top + px(ROW),
    };
    builder.text(
        status,
        RECT {
            left: right - status_width - px(2),
            ..row
        },
        Font::Small,
        theme::MUTED,
        DT_RIGHT | LINE,
    );
    builder.text(
        &input.name,
        RECT {
            right: right - status_width - px(16),
            ..row
        },
        Font::Body,
        ink,
        DT_LEFT | LINE | DT_END_ELLIPSIS,
    );
}

/// The name box for a new or renamed list, with Cancel and Create or Save
/// under it, and why the name won't do.
fn naming_rows(builder: &mut Builder, naming: &Naming, left: i32, right: i32, top: i32) {
    let title = if naming.list.is_some() {
        "Rename priority list"
    } else {
        "New priority list"
    };
    let line = RECT {
        left,
        top,
        right,
        bottom: top + px(44),
    };
    builder.text(title, line, Font::Body, theme::INK, DT_LEFT | LINE);
    let middle = (line.top + line.bottom) / 2;
    let label = theme::text_width(title, Font::Body);
    let field = RECT {
        left: (left + label + px(32)).max(right - px(340)),
        top: middle - px(11),
        right,
        bottom: middle + px(11),
    };
    builder.push(Item::Field {
        id: LIST_NAME,
        area: RECT {
            left: field.left - px(8),
            top: field.top - px(5),
            right: field.right + px(8),
            bottom: field.bottom + px(5),
        },
    });
    builder.place(LIST_NAME, Control::Edit, field, theme::SURFACE);

    let middle = top + px(44) + px(NAMING - 44) / 2;
    let action = if naming.list.is_some() {
        "Save"
    } else {
        "Create"
    };
    let save = builder.place_right(
        NAME_SAVE,
        Control::button(action).enabled(!naming.text.trim().is_empty()),
        right + px(4),
        middle,
        theme::SURFACE,
    );
    let cancel = builder.place_right(
        NAME_CANCEL,
        Control::button("Cancel"),
        save.left - px(8),
        middle,
        theme::SURFACE,
    );
    let (text, colour) = match &naming.error {
        Some(error) => (error.as_str(), theme::ORANGE),
        None => (
            "Each list remembers its own microphone order.",
            theme::MUTED,
        ),
    };
    builder.text(
        text,
        RECT {
            left,
            top: middle - px(12),
            right: cancel.left - px(12),
            bottom: middle + px(12),
        },
        Font::Small,
        colour,
        DT_LEFT | LINE | DT_END_ELLIPSIS,
    );
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
    let last = match &context.test {
        None => return,
        Some(Err(reason)) => {
            page.gap(16);
            page.text_card("Test result", "", reason, theme::ORANGE, "");
            return;
        }
        Some(Ok(last)) => last,
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
