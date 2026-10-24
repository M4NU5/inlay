//! The Server preferences page, as on the Mac: the server's models, how it
//! processes dictation, how long it keeps audio, and the dictionary. Every
//! device shares these, so changes collect in a draft and apply on Save,
//! against the revision they were loaded at. When another device saves first,
//! the draft has to be reloaded before it can be saved.
//!
//! The dictionary's text boxes and buttons come and go with its lists and
//! words. Each has a `Slot` with its own control id; the window makes the
//! control when the layout first places it, and sends its edits here.

use super::layout::{Builder, Control, Icon, Item, Row};
use super::{
    ADD_LIST, CORRECTION, Context, DISCARD, DYNAMIC, KEEP_ORIGINAL, LANGUAGE, PROMPT, PURGE_ALL,
    PURGE_ORIGINAL, RELOAD, RESET_PROMPT, RETENTION, SAVE_SHARED, WM_SERVER, history, server_line,
};
use crate::api::{
    self, Api, ApiError, DictionaryEntry, DictionaryList, Health, Runtime, Shared,
    SharedPreferences, Storage,
};
use crate::app::{self, Server};
use crate::theme::{self, Font, px};
use crate::win::wide;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::thread;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{DT_LEFT, DT_SINGLELINE, DT_VCENTER};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW, PostMessageW,
};

/// The proofreading languages, in the Mac's order. The codes are the server's
/// closed list; other Parakeet languages use Automatic.
pub const LANGUAGES: [(&str, &str); 17] = [
    ("English", "en"),
    ("Automatic", "auto"),
    ("Spanish", "es"),
    ("French", "fr"),
    ("German", "de"),
    ("Italian", "it"),
    ("Portuguese", "pt"),
    ("Dutch", "nl"),
    ("Polish", "pl"),
    ("Russian", "ru"),
    ("Ukrainian", "uk"),
    ("Swedish", "sv"),
    ("Japanese (proofreading only)", "ja"),
    ("Chinese (proofreading only)", "zh"),
    ("Korean (proofreading only)", "ko"),
    ("Hindi (proofreading only)", "hi"),
    ("Arabic (proofreading only)", "ar"),
];

/// How long the server keeps recording audio. 0 keeps it forever.
pub const RETENTIONS: [(&str, u32); 5] = [
    ("7 days", 7),
    ("30 days", 30),
    ("90 days", 90),
    ("1 year", 365),
    ("Forever", 0),
];

const MAXIMUM_LISTS: usize = 32;
const MAXIMUM_ENTRIES: usize = 500;
const MAXIMUM_ALIASES: usize = 8;
const MAXIMUM_PROMPT_BYTES: usize = 4096;
const LINE: u32 = DT_SINGLELINE | DT_VCENTER;

/// A dictionary control, by the list or word it belongs to.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Slot {
    Toggle(String),
    Name(String),
    AddWord(String),
    RemoveList(String),
    Term(String),
    Aliases(String),
    Star(String),
    Remove(String),
}

#[derive(Default)]
struct Page {
    /// The snapshot the draft started from; a save names its revision.
    base: Option<Shared>,
    /// The newest snapshot the server has sent.
    latest: Option<Shared>,
    draft: Option<SharedPreferences>,
    health: Option<Health>,
    storage: Option<Storage>,
    loading: bool,
    /// The server address the state came from.
    endpoint: String,
    /// Counts fresh starts, so a reply from before one is dropped.
    session: u64,
    /// Counts loads, so a reply to an older one is dropped.
    request: u64,
    saving: bool,
    /// The last load or save's failure.
    message: Option<String>,
    /// Lists showing their words, by id.
    expanded: Vec<String>,
    slots: HashMap<Slot, i32>,
    by_id: Vec<Slot>,
    /// The text boxes no longer show the draft: the window remakes them.
    resync: bool,
    /// The server's state when the page last loaded.
    server: Option<Server>,
    /// The last load reached the server with this PC's credential. Saving
    /// and deleting audio need only that, as on the Mac, not the models: a
    /// server short of disk space isn't ready, and deleting audio frees it.
    reachable: bool,
}

static PAGE: LazyLock<Mutex<Page>> = LazyLock::new(Mutex::default);

fn page() -> MutexGuard<'static, Page> {
    PAGE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn post(window: usize) {
    unsafe { PostMessageW(window as HWND, WM_SERVER, 0, 0) };
}

/// The page was opened: load the newest preferences, models and storage.
pub fn opened(window: HWND) {
    page().server = Some(app::shared().status(|status| status.server.clone()));
    load(window);
}

/// Loads again when the server's state changes, to learn whether it still
/// answers.
pub fn catch_up(window: HWND, server: &Server) {
    let changed = {
        let mut page = page();
        let changed = *server != Server::Checking && page.server.as_ref() != Some(server);
        if changed {
            page.server = Some(server.clone());
        }
        changed
    };
    if changed {
        load(window);
    }
}

/// The window closed: start afresh next time.
pub fn closed() {
    reset(&mut page(), String::new());
}

/// Drops everything from the last session, keeping the counters so its
/// replies can't pass for this one's.
fn reset(page: &mut Page, endpoint: String) {
    *page = Page {
        endpoint,
        session: page.session + 1,
        request: page.request,
        resync: true,
        ..Page::default()
    };
}

/// Fetches the server's state. The reply arrives as `WM_SERVER`.
fn load(window: HWND) {
    let settings = app::shared().settings();
    let (request, session) = {
        let mut page = page();
        // A draft from another server must never be saved to this one.
        if page.endpoint != settings.config.endpoint {
            reset(&mut page, settings.config.endpoint.clone());
        }
        page.loading = true;
        page.request += 1;
        (page.request, page.session)
    };
    let target = window as usize;
    thread::spawn(move || {
        let api = Api::new(&settings.config.endpoint, &settings.token);
        let (health, preferences, storage) = match &api {
            Ok(api) => (
                api.health().ok(),
                api.preferences().map_err(|error| error.to_string()),
                api.storage().ok(),
            ),
            Err(message) => (None, Err(message.clone()), None),
        };
        {
            let mut page = page();
            if page.request != request || page.session != session {
                return;
            }
            page.loading = false;
            page.reachable = preferences.is_ok()
                && health
                    .as_ref()
                    .is_some_and(|health| health.api_version == api::API_VERSION);
            if health.is_some() {
                page.health = health;
            }
            if storage.is_some() {
                page.storage = storage;
            }
            match preferences {
                Ok(latest) => {
                    // A save may have finished while this load was out.
                    if page
                        .latest
                        .as_ref()
                        .is_none_or(|current| latest.revision >= current.revision)
                    {
                        page.latest = Some(latest);
                    }
                    page.message = None;
                }
                Err(message) => page.message = Some(message),
            }
        }
        post(target);
    });
}

fn dirty(page: &Page) -> bool {
    match (&page.base, &page.draft) {
        (Some(base), Some(draft)) => base.preferences != *draft,
        _ => false,
    }
}

/// Another device saved since the draft was loaded, and the draft differs.
fn changed_remotely(page: &Page) -> bool {
    match (&page.base, &page.latest) {
        (Some(base), Some(latest)) => dirty(page) && base.revision != latest.revision,
        _ => false,
    }
}

/// Starts the draft again from the newest snapshot.
fn adopt(page: &mut Page) {
    let Some(latest) = page.latest.clone() else {
        return;
    };
    if page.draft.as_ref() != Some(&latest.preferences) {
        page.draft = Some(latest.preferences.clone());
        page.slots.clear();
        page.by_id.clear();
        page.resync = true;
    }
    page.base = Some(latest);
}

/// Takes a newer snapshot when it can't lose an edit, as the Mac does.
fn catch_up_draft(page: &mut Page) {
    let Some(latest) = &page.latest else {
        return;
    };
    let newer = page
        .base
        .as_ref()
        .is_none_or(|base| base.revision < latest.revision);
    if newer && (!dirty(page) || page.draft.as_ref() == Some(&latest.preferences)) {
        adopt(page);
    }
}

/// When the text boxes need remaking from the draft, the cleanup
/// instructions to show in theirs.
pub fn take_resync() -> Option<String> {
    let mut page = page();
    if !page.resync {
        return None;
    }
    page.resync = false;
    Some(
        page.draft
            .as_ref()
            .map(|draft| draft.proofreading_prompt.clone())
            .unwrap_or_default(),
    )
}

/// The control id for `slot`, made up the first time it is asked for.
fn id(page: &mut Page, slot: Slot) -> i32 {
    if let Some(id) = page.slots.get(&slot) {
        return *id;
    }
    let id = DYNAMIC + page.by_id.len() as i32;
    page.by_id.push(slot.clone());
    page.slots.insert(slot, id);
    id
}

fn slot(page: &Page, id: i32) -> Option<Slot> {
    page.by_id.get(usize::try_from(id - DYNAMIC).ok()?).cloned()
}

fn entry_mut<'a>(draft: &'a mut SharedPreferences, id: &str) -> Option<&'a mut DictionaryEntry> {
    draft
        .dictionary
        .lists
        .iter_mut()
        .flat_map(|list| list.entries.iter_mut())
        .find(|entry| entry.id == id)
}

fn list_mut<'a>(draft: &'a mut SharedPreferences, id: &str) -> Option<&'a mut DictionaryList> {
    draft.dictionary.lists.iter_mut().find(|list| list.id == id)
}

/// How a new dictionary text box starts.
#[derive(Default)]
pub struct TextBox {
    pub text: String,
    /// Grey hint text while it is empty.
    pub cue: &'static str,
    /// In the small font, as the Mac's captions.
    pub small: bool,
    /// Right-aligned, as a text box beside a label.
    pub right: bool,
}

pub fn text_box(id: i32) -> Option<TextBox> {
    let mut page = page();
    let slot = slot(&page, id)?;
    let draft = page.draft.as_mut()?;
    match slot {
        Slot::Name(list) => list_mut(draft, &list).map(|list| TextBox {
            text: list.name.clone(),
            cue: "List name",
            right: true,
            ..TextBox::default()
        }),
        Slot::Term(entry) => entry_mut(draft, &entry).map(|entry| TextBox {
            text: entry.term.clone(),
            cue: "Preferred spelling",
            ..TextBox::default()
        }),
        Slot::Aliases(entry) => entry_mut(draft, &entry).map(|entry| TextBox {
            text: entry.aliases.join(", "),
            cue: "Words or phrases to replace, separated by commas",
            small: true,
            ..TextBox::default()
        }),
        _ => None,
    }
}

/// A text box's text changed. Returns whether the draft did.
pub fn edited(id: i32, text: &str) -> bool {
    let mut page = page();
    let slot = if id == PROMPT {
        None
    } else {
        match slot(&page, id) {
            Some(slot) => Some(slot),
            None => return false,
        }
    };
    let Some(draft) = page.draft.as_mut() else {
        return false;
    };
    let field = match &slot {
        None => Some(&mut draft.proofreading_prompt),
        Some(Slot::Name(list)) => list_mut(draft, list).map(|list| &mut list.name),
        Some(Slot::Term(entry)) => entry_mut(draft, entry).map(|entry| &mut entry.term),
        Some(Slot::Aliases(entry)) => {
            let aliases: Vec<String> = text
                .split(',')
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .map(String::from)
                .collect();
            return match entry_mut(draft, entry) {
                Some(entry) if entry.aliases != aliases => {
                    entry.aliases = aliases;
                    true
                }
                _ => false,
            };
        }
        Some(_) => None,
    };
    match field {
        Some(field) if field != text => {
            *field = text.to_string();
            true
        }
        _ => false,
    }
}

/// Acts on a dictionary button. Returns whether it was one.
pub fn command(id: i32) -> bool {
    let mut page = page();
    let Some(slot) = slot(&page, id) else {
        return false;
    };
    if let Slot::Toggle(list) = &slot {
        match page.expanded.iter().position(|open| open == list) {
            Some(index) => {
                page.expanded.remove(index);
            }
            None => page.expanded.push(list.clone()),
        }
        return true;
    }
    let Some(draft) = page.draft.as_mut() else {
        return true;
    };
    match slot {
        Slot::AddWord(list) => {
            if let Some(list) = list_mut(draft, &list) {
                list.entries.push(DictionaryEntry {
                    id: new_id(),
                    term: String::new(),
                    aliases: Vec::new(),
                    is_priority: false,
                    other: Default::default(),
                });
            }
        }
        Slot::RemoveList(list) => draft.dictionary.lists.retain(|each| each.id != list),
        Slot::Star(entry) => {
            if let Some(entry) = entry_mut(draft, &entry) {
                entry.is_priority = !entry.is_priority;
            }
        }
        Slot::Remove(entry) => {
            for list in &mut draft.dictionary.lists {
                list.entries.retain(|each| each.id != entry);
            }
        }
        _ => {}
    }
    true
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

pub fn add_list() {
    let mut page = page();
    let Some(draft) = page.draft.as_mut() else {
        return;
    };
    let list = DictionaryList {
        id: new_id(),
        name: "New list".into(),
        entries: Vec::new(),
        other: Default::default(),
    };
    let id = list.id.clone();
    draft.dictionary.lists.push(list);
    page.expanded.push(id);
}

pub fn toggle_correction() {
    if let Some(draft) = page().draft.as_mut() {
        draft.text_correction_enabled = !draft.text_correction_enabled;
    }
}

pub fn toggle_keep_original() {
    if let Some(draft) = page().draft.as_mut() {
        draft.keep_original_audio = !draft.keep_original_audio;
    }
}

/// Puts back the server's built-in cleanup instructions, and returns them
/// for the text box.
pub fn reset_prompt() -> Option<String> {
    let mut page = page();
    let draft = page.draft.as_mut()?;
    draft.proofreading_prompt = api::DEFAULT_PROMPT.into();
    Some(draft.proofreading_prompt.clone())
}

/// Discard changes, or Reload after another device saved.
pub fn discard() {
    let mut page = page();
    page.message = None;
    adopt(&mut page);
}

/// The labels for the language or retention popup, and which is current.
pub fn choices(id: i32) -> (Vec<String>, Option<usize>) {
    let page = page();
    let Some(draft) = &page.draft else {
        return (Vec::new(), None);
    };
    if id == LANGUAGE {
        let labels = LANGUAGES
            .iter()
            .map(|(label, _)| label.to_string())
            .collect();
        let selected = LANGUAGES
            .iter()
            .position(|(_, code)| *code == draft.language);
        (labels, selected)
    } else {
        let labels = RETENTIONS
            .iter()
            .map(|(label, _)| label.to_string())
            .collect();
        let selected = RETENTIONS
            .iter()
            .position(|(_, days)| *days == draft.audio_retention_days);
        (labels, selected)
    }
}

pub fn choose_language(index: usize) {
    if let (Some(draft), Some((_, code))) = (page().draft.as_mut(), LANGUAGES.get(index)) {
        draft.language = (*code).into();
    }
}

pub fn choose_retention(index: usize) {
    if let (Some(draft), Some((_, days))) = (page().draft.as_mut(), RETENTIONS.get(index)) {
        draft.audio_retention_days = *days;
    }
}

/// Saves the draft over the revision it was loaded at.
pub fn save(window: HWND) {
    let settings = app::shared().settings();
    let (shared, session) = {
        let mut page = page();
        let (Some(base), Some(draft)) = (&page.base, &page.draft) else {
            return;
        };
        if page.saving
            || !page.reachable
            || page.endpoint != settings.config.endpoint
            || !dirty(&page)
            || changed_remotely(&page)
            || validation(draft).is_some()
        {
            return;
        }
        let shared = Shared {
            revision: base.revision,
            preferences: draft.clone(),
        };
        page.saving = true;
        page.message = None;
        (shared, page.session)
    };
    let target = window as usize;
    thread::spawn(move || {
        let mut stale = false;
        let result = Api::new(&settings.config.endpoint, &settings.token).and_then(|api| {
            api.save_preferences(&shared).map_err(|error| {
                stale = matches!(error, ApiError::Rejected { status: 409, .. });
                error.to_string()
            })
        });
        {
            let mut page = page();
            if page.session != session {
                return;
            }
            page.saving = false;
            match result {
                Ok(saved) => {
                    log!("settings: saved shared preferences");
                    // Edits made while saving stay in the draft. A server that
                    // tidied what it was sent replaces the draft it came from.
                    if page.draft.as_ref() == Some(&shared.preferences)
                        && saved.preferences != shared.preferences
                    {
                        page.draft = Some(saved.preferences.clone());
                        page.slots.clear();
                        page.by_id.clear();
                        page.resync = true;
                    }
                    if page
                        .latest
                        .as_ref()
                        .is_none_or(|current| saved.revision >= current.revision)
                    {
                        page.latest = Some(saved.clone());
                    }
                    page.base = Some(saved);
                }
                Err(message) => page.message = Some(message),
            }
        }
        if stale {
            // Fetch what the other device saved, so Reload can show it.
            load(target as HWND);
        }
        post(target);
    });
}

/// Asks, then deletes original audio, or with `all` all recording audio,
/// from every finished take on the server.
pub fn purge(window: HWND, all: bool) {
    let (question, detail) = if all {
        (
            "Delete all recording audio?",
            "Transcripts stay in history. This cannot be undone.",
        )
    } else {
        (
            "Delete all original audio?",
            "Transcripts and transcription audio stay. This cannot be undone.",
        )
    };
    let (text, title) = (wide(&format!("{question}\n\n{detail}")), wide("Inlay"));
    let answer = unsafe {
        MessageBoxW(
            window,
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if answer != IDYES {
        return;
    }
    let settings = app::shared().settings();
    let session = page().session;
    let target = window as usize;
    thread::spawn(move || {
        let kind = if all { None } else { Some("original") };
        let result = Api::new(&settings.config.endpoint, &settings.token).and_then(|api| {
            api.purge_audio(kind).map_err(|error| error.to_string())?;
            Ok(api.storage().ok())
        });
        {
            let mut page = page();
            if page.session != session {
                return;
            }
            match result {
                Ok(storage) => {
                    log!("settings: deleted recording audio on the server");
                    if storage.is_some() {
                        page.storage = storage;
                    }
                }
                Err(message) => page.message = Some(message),
            }
        }
        post(target);
    });
}

/// Why the draft can't be saved, in the Mac's words, or `None`. The server
/// checks again; this keeps Save off until it would pass.
fn validation(draft: &SharedPreferences) -> Option<String> {
    if !LANGUAGES.iter().any(|(_, code)| *code == draft.language) {
        return Some("Choose a supported language.".into());
    }
    let prompt = &draft.proofreading_prompt;
    if prompt.trim().is_empty() {
        return Some("The cleanup system prompt cannot be empty.".into());
    }
    if prompt.len() > MAXIMUM_PROMPT_BYTES || prompt.contains('\0') {
        return Some(
            "The cleanup system prompt must fit within 4 KB and contain no null characters.".into(),
        );
    }
    let lists = &draft.dictionary.lists;
    if lists.len() > MAXIMUM_LISTS {
        return Some("The dictionary can contain up to 32 lists.".into());
    }
    if !unique(lists.iter().map(|list| list.id.as_str())) {
        return Some("Dictionary list ids must be unique.".into());
    }
    for list in lists {
        if let Some(error) = list_error(list) {
            return Some(error);
        }
    }
    let entries: Vec<&DictionaryEntry> = lists.iter().flat_map(|list| &list.entries).collect();
    if entries.len() > MAXIMUM_ENTRIES {
        return Some("The dictionary can contain up to 500 words in total.".into());
    }
    if !unique(entries.iter().map(|entry| entry.id.as_str())) {
        return Some("Dictionary word ids must be unique across all lists.".into());
    }
    let mut spellings: HashMap<String, &str> = HashMap::new();
    for entry in entries {
        for spelling in std::iter::once(&entry.term).chain(&entry.aliases) {
            let key = fold(spelling);
            // The server compares the terms after NFC normalization, which
            // this can't do, so it judges clashes between non-ASCII terms.
            if let Some(existing) = spellings.get(&key)
                && *existing != entry.term
                && existing.is_ascii()
                && entry.term.is_ascii()
            {
                return Some(format!(
                    "{spelling} maps to both {existing} and {}. Each spelling can have only one preferred word across all lists.",
                    entry.term
                ));
            }
            spellings.insert(key, &entry.term);
        }
    }
    None
}

fn list_error(list: &DictionaryList) -> Option<String> {
    if !valid_text(&list.id) {
        return Some("Each dictionary list needs a nonempty id of at most 128 characters.".into());
    }
    if !valid_text(&list.name) {
        return Some("Dictionary list names must be single-line, nonempty text of at most 80 characters, without surrounding whitespace.".into());
    }
    if list.entries.len() > MAXIMUM_ENTRIES {
        return Some("The dictionary can contain up to 500 words in total.".into());
    }
    if !unique(list.entries.iter().map(|entry| entry.id.as_str())) {
        return Some("Dictionary word ids must be unique.".into());
    }
    list.entries.iter().find_map(entry_error)
}

fn entry_error(entry: &DictionaryEntry) -> Option<String> {
    if !valid_text(&entry.id) {
        return Some("Each dictionary word needs a nonempty id of at most 128 characters.".into());
    }
    if !valid_text(&entry.term) {
        return Some("Dictionary words must be single-line, nonempty text of at most 128 characters, without surrounding whitespace.".into());
    }
    if entry.aliases.len() > MAXIMUM_ALIASES {
        return Some("Each dictionary word can have up to 8 corrections.".into());
    }
    let mut seen = vec![fold(&entry.term)];
    for alias in &entry.aliases {
        if !valid_text(alias) {
            return Some("Corrections must be single-line, nonempty text of at most 128 characters, without surrounding whitespace.".into());
        }
        let key = fold(alias);
        if seen.contains(&key) {
            return Some(format!(
                "Corrections for {} must be unique and different from its preferred spelling. Capitalization is corrected automatically.",
                entry.term
            ));
        }
        seen.push(key);
    }
    None
}

/// The server's text rule, except the length: it counts graphemes, which a
/// count here could only overstate, so it is left to the server.
fn valid_text(text: &str) -> bool {
    !text.is_empty() && text == text.trim() && !text.chars().any(char::is_control)
}

/// The server's case folding (`caseFold` in `Server/src/domain/dictionary.ts`),
/// so spellings that differ only in case are the same word. The server also
/// normalizes to NFC; spellings that differ only in that are left to it.
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(|character| -> Vec<char> {
            match character as u32 {
                0x131 => vec![character],
                0x1E9E => vec!['s', 's'],
                0x13A0..=0x13F5 | 0x13F8..=0x13FD | 0xAB70..=0xABBF => {
                    character.to_uppercase().collect()
                }
                _ => character
                    .to_uppercase()
                    .flat_map(char::to_lowercase)
                    .collect(),
            }
        })
        .collect()
}

fn unique<'a>(ids: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().all(|id| seen.insert(id))
}

pub fn build(builder: &mut Builder, context: &Context) {
    let mut page = page();
    catch_up_draft(&mut page);
    builder.title("Server preferences");

    let available = page.reachable && page.latest.is_some();
    let dirty = dirty(&page);
    let remote = changed_remotely(&page);
    let invalid = page.draft.as_ref().and_then(validation);

    let (dot, status) = server_line(&context.server);
    builder.status(dot, status, &context.settings.config.endpoint, None);
    builder.gap(12);
    let save = Control::primary("Save shared preferences", Icon::Check)
        .enabled(available && dirty && !remote && invalid.is_none() && !page.saving);
    let mut actions = vec![
        (SAVE_SHARED, save),
        (DISCARD, Control::button("Discard changes").enabled(dirty)),
    ];
    if remote {
        actions.push((RELOAD, Control::button("Reload")));
    }
    builder.actions(actions);
    let message = if page.latest.is_none() && page.loading {
        Some("Loading shared preferences…".to_string())
    } else if !page.reachable {
        Some("Cannot load shared preferences while the server is unavailable.".to_string())
    } else if remote {
        Some("Shared preferences changed on another device. Reload to continue.".to_string())
    } else {
        invalid.or_else(|| page.message.clone())
    };
    if let Some(message) = message {
        builder.note(&message, theme::ORANGE);
    }
    builder.gap(26);

    let Some(draft) = page.draft.clone() else {
        return;
    };

    if page.reachable
        && let Some(health) = &page.health
        && let (Some(speech), Some(proofreading)) = (&health.speech, &health.proofreading)
    {
        builder.section("Server models");
        let mut rows = vec![
            runtime_row("Voice", speech),
            runtime_row("Proofreading", proofreading),
        ];
        if let Some(version) = &health.server_version {
            rows.push(Row::new("Server version").value(version));
        }
        builder.card(rows);
        builder.gap(26);
    }

    builder.section("Processing");
    let language = LANGUAGES
        .iter()
        .find(|(_, code)| *code == draft.language)
        .map_or(draft.language.as_str(), |(label, _)| label);
    builder.card(vec![
        Row::new("Proofreading language").control(LANGUAGE, Control::choice(language, false)),
        Row::caption(
            "Parakeet recognizes 25 European languages automatically. Use Automatic for other proofreading languages.",
        ),
        Row::new("Proofread with Qwen").control(
            CORRECTION,
            Control::Switch {
                on: draft.text_correction_enabled,
            },
        ),
        Row::new("Cleanup instructions").control(
            RESET_PROMPT,
            Control::button("Reset to default")
                .enabled(draft.proofreading_prompt != api::DEFAULT_PROMPT),
        ),
    ]);
    builder.gap(8);
    builder.editor(PROMPT, 300);
    builder.gap(26);

    builder.section("Shared history");
    let retention = RETENTIONS
        .iter()
        .find(|(_, days)| *days == draft.audio_retention_days)
        .map_or_else(
            || format!("{} days", draft.audio_retention_days),
            |(label, _)| label.to_string(),
        );
    let mut rows = vec![
        Row::new("Keep original microphone audio").control(
            KEEP_ORIGINAL,
            Control::Switch {
                on: draft.keep_original_audio,
            },
        ),
        Row::new("Keep audio").control(RETENTION, Control::choice(&retention, false)),
        Row::caption(
            "Transcription and original audio are removed after this period; text is always kept. Keeping originals also saves the original microphone audio for future dictations.",
        ),
    ];
    if let Some(usage) = &page.storage {
        rows.push(Row::new("Recording audio").value(&bytes(usage.audio_byte_count)));
        rows.push(Row::new("Transcription audio").value(&bytes(usage.inference_byte_count)));
        rows.push(Row::new("Original audio").value(&bytes(usage.original_byte_count)));
        rows.push(Row::new("Recordings").value(&usage.generation_count.to_string()));
        if let Some(oldest) = usage
            .oldest_audio_at
            .as_deref()
            .and_then(history::day_label)
        {
            rows.push(Row::new("Oldest audio").value(&oldest));
        }
        rows.push(Row::new("Free on server").value(&bytes(usage.free_byte_count)));
    }
    builder.card(rows);
    if page.storage.is_some() {
        builder.gap(12);
        builder.actions(vec![
            (
                PURGE_ORIGINAL,
                Control::button("Delete original audio").enabled(available),
            ),
            (
                PURGE_ALL,
                Control::button("Delete all audio").enabled(available),
            ),
        ]);
    }
    builder.gap(26);

    builder.section("Dictionary");
    if !draft.dictionary.lists.is_empty() {
        dictionary(builder, &mut page, &draft);
        builder.gap(12);
    }
    builder.actions(vec![(ADD_LIST, Control::button("Add list"))]);
    builder.note(
        "Use narrow phrases: preferred ‘auth middleware’, replace ‘off middleware’. Replacing ‘off’ alone also changes ordinary uses of that word. Starred words are suggested first when model space is limited.",
        theme::MUTED,
    );
}

fn runtime_row(title: &str, runtime: &Runtime) -> Row {
    let colour = if runtime.ready {
        theme::GREEN
    } else {
        theme::RED
    };
    Row::new(title)
        .dot(colour)
        .detail(runtime.message.as_deref().unwrap_or(&runtime.backend))
        .value(&runtime.model_id)
}

/// A byte count as Finder shows it, in powers of 1000.
fn bytes(count: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    match count {
        0 => return "Zero KB".into(),
        1..1000 => return format!("{count} bytes"),
        _ => {}
    }
    let mut value = count as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} KB")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// The dictionary card's rows, in logical pixels.
const HEADER: i32 = 46;
const NAME_ROW: i32 = 46;
const ENTRY_ROW: i32 = 74;
const FOOTER: i32 = 52;
const INSET: i32 = 16;

/// Every list in one card, each a row that expands to its name and words,
/// as the Mac's disclosure groups.
fn dictionary(builder: &mut Builder, page: &mut Page, draft: &SharedPreferences) {
    let expanded = page.expanded.clone();
    let open = |list: &DictionaryList| expanded.contains(&list.id);
    let heights: Vec<i32> = draft
        .dictionary
        .lists
        .iter()
        .map(|list| {
            let mut height = px(HEADER);
            if open(list) {
                height += px(NAME_ROW) + 1;
                height += list.entries.len() as i32 * (px(ENTRY_ROW) + 1);
                height += px(FOOTER) + 1;
            }
            height
        })
        .collect();
    let rules = draft.dictionary.lists.len() as i32 - 1;
    let total = heights.iter().sum::<i32>() + rules.max(0);
    let card = builder.band(total);
    builder.push(Item::Card(card));
    let (left, right) = (card.left + px(INSET), card.right - px(INSET));
    let mut y = card.top;
    let rule = |builder: &mut Builder, y: &mut i32| {
        builder.push(Item::Rule(RECT {
            left,
            top: *y,
            right,
            bottom: *y + 1,
        }));
        *y += 1;
    };
    for (index, list) in draft.dictionary.lists.iter().enumerate() {
        if index > 0 {
            rule(builder, &mut y);
        }
        let expanded = open(list);
        let header = RECT {
            left: left - px(8),
            top: y + px(4),
            right: right + px(8),
            bottom: y + px(HEADER) - px(4),
        };
        let name = if list.name.is_empty() {
            "New list"
        } else {
            &list.name
        };
        let toggle = id(page, Slot::Toggle(list.id.clone()));
        builder.place(
            toggle,
            Control::Disclosure {
                label: name.into(),
                count: list.entries.len().to_string(),
                open: expanded,
            },
            header,
            theme::SURFACE,
        );
        y += px(HEADER);
        if !expanded {
            continue;
        }

        rule(builder, &mut y);
        let row = RECT {
            left,
            top: y,
            right,
            bottom: y + px(NAME_ROW),
        };
        builder.text("List name", row, Font::Body, theme::INK, DT_LEFT | LINE);
        let middle = (row.top + row.bottom) / 2;
        let label = theme::text_width("List name", Font::Body);
        let field = RECT {
            left: (left + label + px(32)).max(right - px(340)),
            top: middle - px(11),
            right,
            bottom: middle + px(11),
        };
        let name_id = id(page, Slot::Name(list.id.clone()));
        field_box(builder, name_id, field);
        y += px(NAME_ROW);

        for entry in &list.entries {
            rule(builder, &mut y);
            let word = if entry.term.is_empty() {
                "word"
            } else {
                &entry.term
            };
            let middle = y + px(ENTRY_ROW) / 2;
            let remove = id(page, Slot::Remove(entry.id.clone()));
            let remove_area = builder.place_right(
                remove,
                Control::icon(Icon::Remove, &format!("Remove {word}")),
                right,
                middle,
                theme::SURFACE,
            );
            let star = id(page, Slot::Star(entry.id.clone()));
            let star_icon = if entry.is_priority {
                Icon::StarFilled
            } else {
                Icon::Star
            };
            let star_area = builder.place_right(
                star,
                Control::icon(star_icon, &format!("Prioritize {word}")),
                remove_area.left - px(4),
                middle,
                theme::SURFACE,
            );
            let edge = star_area.left - px(16);
            let term = RECT {
                left,
                top: y + px(13),
                right: edge,
                bottom: y + px(35),
            };
            let term_id = id(page, Slot::Term(entry.id.clone()));
            field_box(builder, term_id, term);
            let aliases = RECT {
                left,
                top: y + px(43),
                right: edge,
                bottom: y + px(63),
            };
            let aliases_id = id(page, Slot::Aliases(entry.id.clone()));
            field_box(builder, aliases_id, aliases);
            y += px(ENTRY_ROW);
        }

        rule(builder, &mut y);
        let middle = y + px(FOOTER) / 2;
        let add = id(page, Slot::AddWord(list.id.clone()));
        let width = Control::button("Add word").width();
        builder.place(
            add,
            Control::button("Add word"),
            RECT {
                left: left - px(4),
                top: middle - px(15),
                right: left - px(4) + width,
                bottom: middle + px(15),
            },
            theme::SURFACE,
        );
        let remove = id(page, Slot::RemoveList(list.id.clone()));
        builder.place_right(
            remove,
            Control::button("Remove list"),
            right + px(4),
            middle,
            theme::SURFACE,
        );
        y += px(FOOTER);
    }
}

/// A borderless text box with the field drawn behind it while it has the focus.
fn field_box(builder: &mut Builder, id: i32, area: RECT) {
    builder.push(Item::Field {
        id,
        area: RECT {
            left: area.left - px(8),
            top: area.top - px(5),
            right: area.right + px(8),
            bottom: area.bottom + px(5),
        },
    });
    builder.place(id, Control::Edit, area, theme::SURFACE);
}
