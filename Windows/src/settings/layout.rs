//! Page layout for the main window. A page is built top to bottom, like the
//! Mac app's stacks, into a list of painted shapes plus where each of its
//! controls sits. Coordinates are device pixels from the top of the page; the
//! window subtracts its scroll offset when it paints and places them.

use crate::theme::{self, Canvas, Font, px};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_PATH_ELLIPSIS, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK,
};

/// Space between a card's edge and its rows, in logical pixels.
const INSET: i32 = 16;
/// The height of the buttons in a row of actions, in logical pixels.
const ACTION_HEIGHT: i32 = 38;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Microphone,
    Stop,
    Copy,
    Gear,
    Waveform,
    Laptop,
}

impl Icon {
    /// Draws the icon `size` pixels square with its top-left corner at `x`, `y`.
    pub fn draw(self, canvas: &Canvas, x: i32, y: i32, size: i32, rgb: u32) {
        match self {
            Icon::Microphone => canvas.microphone(x, y, size, rgb),
            Icon::Stop => {
                let inset = size * 7 / 24;
                let square = RECT {
                    left: x + inset,
                    top: y + inset,
                    right: x + size - inset,
                    bottom: y + size - inset,
                };
                canvas.round(square, size as f32 / 12.0, rgb);
            }
            Icon::Copy => canvas.copy(x, y, size, rgb),
            Icon::Gear => canvas.gear(x, y, size, rgb),
            Icon::Waveform => canvas.waveform(x, y, size, rgb),
            Icon::Laptop => canvas.laptop(x, y, size, rgb),
        }
    }
}

/// Something painted on the page.
pub enum Item {
    Text {
        text: String,
        area: RECT,
        font: Font,
        colour: u32,
        flags: u32,
    },
    Card(RECT),
    /// A hairline divider.
    Rule(RECT),
    Dot {
        x: i32,
        y: i32,
        colour: u32,
    },
    /// The field behind text box `id`, drawn while it has the focus.
    Field {
        id: i32,
        area: RECT,
    },
    /// The dictation key, drawn as a key cap.
    KeyCap {
        area: RECT,
        label: String,
        pressed: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Primary,
    Secondary,
    /// A square button showing only its icon. Its label is for screen readers.
    Icon,
}

#[derive(Clone)]
pub enum Control {
    /// A borderless text box. Its text lives in the control, not here.
    Edit,
    Choice {
        text: String,
        /// Drawn in a box, as the microphone picker. Otherwise only its value shows.
        boxed: bool,
    },
    Switch {
        on: bool,
    },
    Button {
        label: String,
        style: Style,
        icon: Option<Icon>,
        enabled: bool,
    },
}

impl Control {
    pub fn button(label: &str) -> Control {
        Control::Button {
            label: label.into(),
            style: Style::Secondary,
            icon: None,
            enabled: true,
        }
    }

    pub fn primary(label: &str, icon: Icon) -> Control {
        Control::Button {
            label: label.into(),
            style: Style::Primary,
            icon: Some(icon),
            enabled: true,
        }
    }

    pub fn icon(icon: Icon, label: &str) -> Control {
        Control::Button {
            label: label.into(),
            style: Style::Icon,
            icon: Some(icon),
            enabled: true,
        }
    }

    pub fn choice(text: &str, boxed: bool) -> Control {
        Control::Choice {
            text: text.into(),
            boxed,
        }
    }

    pub fn with_icon(mut self, with: Icon) -> Control {
        if let Control::Button { icon, .. } = &mut self {
            *icon = Some(with);
        }
        self
    }

    pub fn enabled(mut self, value: bool) -> Control {
        if let Control::Button { enabled, .. } = &mut self {
            *enabled = value;
        }
        self
    }

    /// The text a screen reader announces for the control.
    pub fn label(&self) -> Option<&str> {
        match self {
            Control::Button { label, .. } | Control::Choice { text: label, .. } => Some(label),
            Control::Edit | Control::Switch { .. } => None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        !matches!(self, Control::Button { enabled: false, .. })
    }

    /// The control's natural width and height in device pixels.
    fn size(&self) -> (i32, i32) {
        match self {
            Control::Edit => (px(300), px(22)),
            Control::Choice { text, boxed } => {
                let width = theme::text_width(text, Font::Body) + px(if *boxed { 52 } else { 44 });
                (width.clamp(px(120), px(380)), px(30))
            }
            Control::Switch { .. } => (px(38), px(22)),
            Control::Button {
                label, style, icon, ..
            } => match style {
                Style::Icon => (px(40), px(30)),
                _ => {
                    let icon = if icon.is_some() { px(24) } else { 0 };
                    (theme::text_width(label, Font::Body) + icon + px(32), px(30))
                }
            },
        }
    }
}

/// A control and where it sits on the page.
#[derive(Clone)]
pub struct Placed {
    pub id: i32,
    pub control: Control,
    pub area: RECT,
    /// The colour behind the control, which owner-drawn buttons fill first.
    pub background: u32,
}

pub struct Layout {
    pub items: Vec<Item>,
    pub controls: Vec<Placed>,
    /// The page's full height, which may be taller than the window.
    pub height: i32,
}

impl Layout {
    pub const EMPTY: Layout = Layout {
        items: Vec::new(),
        controls: Vec::new(),
        height: 0,
    };

    pub fn control(&self, id: i32) -> Option<&Placed> {
        self.controls.iter().find(|placed| placed.id == id)
    }
}

/// One line of a card.
pub struct Row {
    label: String,
    strong: bool,
    dot: Option<u32>,
    detail: Option<String>,
    value: Option<String>,
    path: bool,
    control: Option<(i32, Control)>,
    /// A caption across the whole row, wrapped, with no control.
    caption: bool,
}

impl Row {
    pub fn new(label: &str) -> Row {
        Row {
            label: label.into(),
            strong: false,
            dot: None,
            detail: None,
            value: None,
            path: false,
            control: None,
            caption: false,
        }
    }

    pub fn caption(text: &str) -> Row {
        Row {
            caption: true,
            ..Row::new(text)
        }
    }

    pub fn strong(self) -> Row {
        Row {
            strong: true,
            ..self
        }
    }

    /// A status dot before the label.
    pub fn dot(self, colour: u32) -> Row {
        Row {
            dot: Some(colour),
            ..self
        }
    }

    /// Muted text on the right, before any control.
    pub fn value(self, text: &str) -> Row {
        Row {
            value: Some(text.into()),
            ..self
        }
    }

    /// Shortens the value in its middle, as a file path.
    pub fn path(self) -> Row {
        Row { path: true, ..self }
    }

    pub fn control(self, id: i32, control: Control) -> Row {
        Row {
            control: Some((id, control)),
            ..self
        }
    }

    fn height(&self, width: i32) -> i32 {
        if self.caption {
            theme::text_height(&self.label, Font::Small, width, wrapped()) + px(24)
        } else if self.detail.is_some() {
            px(58)
        } else {
            px(44)
        }
    }
}

fn wrapped() -> u32 {
    DT_LEFT | DT_WORDBREAK | DT_NOPREFIX
}

const LINE: u32 = DT_SINGLELINE | DT_VCENTER;

pub struct Builder {
    left: i32,
    right: i32,
    y: i32,
    items: Vec<Item>,
    controls: Vec<Placed>,
}

impl Builder {
    pub fn new(left: i32, right: i32, top: i32) -> Builder {
        Builder {
            left,
            right,
            y: top,
            items: Vec::new(),
            controls: Vec::new(),
        }
    }

    pub fn finish(mut self) -> Layout {
        self.gap(32);
        Layout {
            items: self.items,
            controls: self.controls,
            height: self.y,
        }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    /// Vertical space, in logical pixels.
    pub fn gap(&mut self, logical: i32) {
        self.y += px(logical);
    }

    /// Takes a band of the page `height` device pixels tall.
    pub fn band(&mut self, height: i32) -> RECT {
        let area = RECT {
            left: self.left,
            top: self.y,
            right: self.right,
            bottom: self.y + height,
        };
        self.y += height;
        area
    }

    pub fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    pub fn text(&mut self, text: &str, area: RECT, font: Font, colour: u32, flags: u32) {
        self.items.push(Item::Text {
            text: text.into(),
            area,
            font,
            colour,
            flags,
        });
    }

    pub fn place(&mut self, id: i32, control: Control, area: RECT, background: u32) {
        self.controls.push(Placed {
            id,
            control,
            area,
            background,
        });
    }

    /// Places a control at its natural size, its right edge at `right` and
    /// centred on `middle`. Returns where it went.
    pub fn place_right(
        &mut self,
        id: i32,
        control: Control,
        right: i32,
        middle: i32,
        background: u32,
    ) -> RECT {
        let (width, height) = control.size();
        let area = RECT {
            left: right - width,
            top: middle - height / 2,
            right,
            bottom: middle - height / 2 + height,
        };
        self.place(id, control, area, background);
        area
    }

    /// A hairline across the page.
    pub fn rule(&mut self) {
        let area = self.band(1);
        self.items.push(Item::Rule(area));
    }

    /// The page heading.
    pub fn title(&mut self, text: &str) {
        let area = self.band(px(40));
        self.text(text, area, Font::Title, theme::INK, DT_LEFT | LINE);
        self.gap(22);
    }

    /// A bold heading above a card.
    pub fn section(&mut self, text: &str) {
        let mut area = self.band(px(22));
        area.left += px(2);
        self.text(text, area, Font::Strong, theme::INK, DT_LEFT | LINE);
        self.gap(10);
    }

    /// Small wrapped text, such as the explanation under a card.
    pub fn note(&mut self, text: &str, colour: u32) {
        self.gap(8);
        let width = self.width() - px(8);
        let height = theme::text_height(text, Font::Small, width, wrapped());
        let mut area = self.band(height);
        area.left += px(4);
        area.right -= px(4);
        self.text(text, area, Font::Small, colour, wrapped());
    }

    /// A status line on the page itself: a coloured dot and text on the left,
    /// muted text and a button on the right.
    pub fn status(&mut self, dot: u32, text: &str, value: &str, button: Option<(i32, Control)>) {
        let area = self.band(px(36));
        let middle = (area.top + area.bottom) / 2;
        let mut right = area.right;
        if let Some((id, control)) = button {
            right = self
                .place_right(id, control, right, middle, theme::CANVAS)
                .left
                - px(14);
        }
        let label = theme::text_width(text, Font::Body);
        let start = area.left + px(18);
        let value_left = (start + label + px(16)).min(right);
        self.text(
            value,
            RECT {
                left: value_left,
                right,
                ..area
            },
            Font::Small,
            theme::MUTED,
            DT_RIGHT | LINE | DT_END_ELLIPSIS,
        );
        self.items.push(Item::Dot {
            x: area.left + px(5),
            y: middle,
            colour: dot,
        });
        self.text(
            text,
            RECT {
                left: start,
                right: value_left,
                ..area
            },
            Font::Body,
            theme::INK,
            DT_LEFT | LINE | DT_END_ELLIPSIS,
        );
    }

    /// A rounded card of rows separated by hairlines.
    pub fn card(&mut self, rows: Vec<Row>) {
        let (left, right) = (self.left + px(INSET), self.right - px(INSET));
        let heights: Vec<i32> = rows.iter().map(|row| row.height(right - left)).collect();
        let top = self.y;
        let bottom = top + heights.iter().sum::<i32>();
        self.items.push(Item::Card(RECT {
            left: self.left,
            top,
            right: self.right,
            bottom,
        }));
        let mut y = top;
        for (index, (row, height)) in rows.into_iter().zip(heights).enumerate() {
            if index > 0 {
                self.items.push(Item::Rule(RECT {
                    left,
                    top: y,
                    right,
                    bottom: y + 1,
                }));
            }
            self.row(
                row,
                RECT {
                    left,
                    top: y,
                    right,
                    bottom: y + height,
                },
            );
            y += height;
        }
        self.y = bottom;
    }

    fn row(&mut self, row: Row, area: RECT) {
        if row.caption {
            let text = RECT {
                top: area.top + px(12),
                bottom: area.bottom - px(12),
                ..area
            };
            self.text(&row.label, text, Font::Small, theme::MUTED, wrapped());
            return;
        }
        let middle = (area.top + area.bottom) / 2;
        let font = if row.strong { Font::Strong } else { Font::Body };
        let mut label_left = area.left;
        if let Some(colour) = row.dot {
            self.items.push(Item::Dot {
                x: label_left + px(5),
                y: middle,
                colour,
            });
            label_left += px(18);
        }
        let label_width = theme::text_width(&row.label, font);
        let mut right = area.right;
        if let Some((id, control)) = row.control {
            if let Control::Edit = control {
                // Text boxes take the room the label leaves, right-aligned as on the Mac.
                let left = (label_left + label_width + px(32)).max(right - px(340));
                let field = RECT {
                    left,
                    top: middle - px(11),
                    right,
                    bottom: middle + px(11),
                };
                self.items.push(Item::Field {
                    id,
                    area: RECT {
                        left: field.left - px(8),
                        top: field.top - px(5),
                        right: field.right + px(8),
                        bottom: field.bottom + px(5),
                    },
                });
                self.place(id, control, field, theme::SURFACE);
                right = field.left - px(16);
            } else {
                let placed = self.place_right(id, control, right, middle, theme::SURFACE);
                right = placed.left - px(12);
            }
        }
        if let Some(value) = &row.value {
            let left = (label_left + label_width + px(16)).min(right);
            let ellipsis = if row.path {
                DT_PATH_ELLIPSIS
            } else {
                DT_END_ELLIPSIS
            };
            self.text(
                value,
                RECT {
                    left,
                    right,
                    ..area
                },
                Font::Body,
                theme::MUTED,
                DT_RIGHT | LINE | ellipsis,
            );
            right = left;
        }
        match &row.detail {
            Some(detail) => {
                let title = RECT {
                    left: label_left,
                    top: area.top + px(9),
                    right,
                    bottom: area.top + px(30),
                };
                self.text(
                    &row.label,
                    title,
                    font,
                    theme::INK,
                    DT_LEFT | LINE | DT_END_ELLIPSIS,
                );
                let below = RECT {
                    top: title.bottom,
                    bottom: title.bottom + px(18),
                    ..title
                };
                self.text(
                    detail,
                    below,
                    Font::Small,
                    theme::MUTED,
                    DT_LEFT | LINE | DT_END_ELLIPSIS,
                );
            }
            None => self.text(
                &row.label,
                RECT {
                    left: label_left,
                    right,
                    ..area
                },
                font,
                theme::INK,
                DT_LEFT | LINE | DT_END_ELLIPSIS,
            ),
        }
    }

    /// A row of buttons. Primary buttons share the width the others leave.
    pub fn actions(&mut self, buttons: Vec<(i32, Control)>) {
        let area = self.band(px(ACTION_HEIGHT));
        let gap = px(12);
        let fixed: i32 = buttons
            .iter()
            .filter(|(_, control)| !is_primary(control))
            .map(|(_, control)| control.size().0)
            .sum();
        let primaries = buttons
            .iter()
            .filter(|(_, control)| is_primary(control))
            .count() as i32;
        let gaps = gap * (buttons.len() as i32 - 1).max(0);
        let stretch = if primaries > 0 {
            (area.right - area.left - fixed - gaps) / primaries
        } else {
            0
        };
        let mut left = area.left;
        for (id, control) in buttons {
            let width = if is_primary(&control) {
                stretch
            } else {
                control.size().0
            };
            let placed = RECT {
                left,
                right: left + width,
                ..area
            };
            self.place(id, control, placed, theme::CANVAS);
            left += width + gap;
        }
    }
}

fn is_primary(control: &Control) -> bool {
    matches!(
        control,
        Control::Button {
            style: Style::Primary,
            ..
        }
    )
}

/// Paints the page's shapes, moved up by `scroll`. `focus` is the id of the
/// control with the keyboard focus.
pub fn paint(canvas: &Canvas, items: &[Item], scroll: i32, focus: i32) {
    let s = theme::scale();
    let shift = |area: &RECT| RECT {
        top: area.top - scroll,
        bottom: area.bottom - scroll,
        ..*area
    };
    for item in items {
        match item {
            Item::Text {
                text,
                area,
                font,
                colour,
                flags,
            } => canvas.text(text, shift(area), *font, *colour, *flags),
            Item::Card(area) => {
                let area = shift(area);
                canvas.round(area, 10.0 * s, theme::SURFACE);
                canvas.outline(area, 10.0 * s, theme::LINE, s);
            }
            Item::Rule(area) => canvas.fill(shift(area), theme::LINE),
            Item::Dot { x, y, colour } => {
                canvas.circle(*x as f32, (y - scroll) as f32, 4.0 * s, *colour)
            }
            Item::Field { id, area } => {
                if *id == focus {
                    let area = shift(area);
                    canvas.round(area, 7.0 * s, theme::FIELD);
                    canvas.outline(area, 7.0 * s, theme::ACCENT, 1.5 * s);
                }
            }
            Item::KeyCap {
                area,
                label,
                pressed,
            } => key_cap(canvas, shift(area), label, *pressed),
        }
    }
}

/// A raised key with its label, pressed down while recording.
fn key_cap(canvas: &Canvas, area: RECT, label: &str, pressed: bool) {
    let s = theme::scale();
    let radius = 12.0 * s;
    let depth = if pressed { px(1) } else { px(4) };
    let edge = RECT {
        top: area.top + px(4),
        bottom: area.bottom + px(4),
        ..area
    };
    canvas.round(edge, radius, theme::SIDEBAR_SELECTED);
    let top = RECT {
        top: area.top + px(4) - depth,
        bottom: area.bottom + px(4) - depth,
        ..area
    };
    canvas.round(top, radius, theme::SURFACE);
    canvas.outline(
        top,
        radius,
        if pressed { theme::ACCENT } else { theme::LINE },
        s,
    );
    let width = top.right - top.left - px(12);
    let height = theme::text_height(
        label,
        Font::Strong,
        width,
        DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
    );
    let middle = (top.top + top.bottom) / 2;
    canvas.text(
        label,
        RECT {
            left: top.left + px(6),
            top: middle - height / 2,
            right: top.right - px(6),
            bottom: middle - height / 2 + height,
        },
        Font::Strong,
        theme::ACCENT,
        DT_CENTER | DT_WORDBREAK,
    );
}

/// How a control looks under the pointer and keyboard.
pub struct Look {
    pub hot: bool,
    pub pressed: bool,
    pub focused: bool,
}

/// Draws an owner-drawn control into its own `area`.
pub fn draw(canvas: &Canvas, area: RECT, placed: &Placed, look: Look) {
    let s = theme::scale();
    canvas.fill(area, placed.background);
    let middle = (area.top + area.bottom) as f32 / 2.0;
    match &placed.control {
        Control::Edit => {}
        Control::Choice { text, boxed } => {
            let fill = if look.hot {
                Some(theme::HOVER)
            } else if *boxed {
                Some(theme::BOX)
            } else {
                None
            };
            if let Some(fill) = fill {
                canvas.round(area, 7.0 * s, fill);
            }
            if look.focused {
                canvas.outline(area, 7.0 * s, theme::ACCENT, 1.5 * s);
            }
            let size = px(22);
            let chevrons = RECT {
                left: area.right - px(4) - size,
                top: (middle as i32) - size / 2,
                right: area.right - px(4),
                bottom: (middle as i32) - size / 2 + size,
            };
            if !*boxed && !look.hot {
                canvas.round(chevrons, 6.0 * s, theme::BOX);
            }
            canvas.updown(
                (chevrons.left + chevrons.right) as f32 / 2.0,
                middle,
                7.0 * s,
                theme::INK,
            );
            canvas.text(
                text,
                RECT {
                    left: area.left + px(10),
                    right: chevrons.left - px(8),
                    ..area
                },
                Font::Body,
                theme::INK,
                (if *boxed { DT_LEFT } else { DT_RIGHT }) | LINE | DT_END_ELLIPSIS,
            );
        }
        Control::Switch { on } => {
            let track = match (*on, look.hot) {
                (true, true) => theme::ACCENT_HOT,
                (true, false) => theme::ACCENT,
                (false, true) => theme::RAISED,
                (false, false) => theme::LINE,
            };
            let radius = (area.bottom - area.top) as f32 / 2.0;
            canvas.round(area, radius, track);
            if look.focused {
                canvas.outline(area, radius, theme::INK, 1.5 * s);
            }
            let knob = radius - 2.5 * s;
            let x = if *on {
                area.right as f32 - 2.5 * s - knob
            } else {
                area.left as f32 + 2.5 * s + knob
            };
            canvas.circle(x, middle, knob, theme::INK);
        }
        Control::Button {
            label,
            style,
            icon,
            enabled,
        } => {
            let on_page = placed.background == theme::CANVAS;
            let (fill, colour, font) = match (style, enabled, look.pressed, look.hot) {
                (Style::Primary, false, ..) => (theme::RAISED, theme::MUTED, Font::Strong),
                (Style::Primary, true, true, _) => {
                    (theme::ACCENT_DOWN, theme::ON_ACCENT, Font::Strong)
                }
                (Style::Primary, true, false, true) => {
                    (theme::ACCENT_HOT, theme::ON_ACCENT, Font::Strong)
                }
                (Style::Primary, true, false, false) => {
                    (theme::ACCENT, theme::ON_ACCENT, Font::Strong)
                }
                (_, false, ..) => (rest(on_page), theme::FAINT, Font::Body),
                (_, true, true, _) => (theme::LINE, theme::ACCENT, Font::Body),
                (_, true, false, true) => (
                    if on_page { theme::HOVER } else { theme::RAISED },
                    theme::ACCENT_HOT,
                    Font::Body,
                ),
                (_, true, false, false) => (rest(on_page), theme::ACCENT, Font::Body),
            };
            canvas.round(area, 8.0 * s, fill);
            if look.focused {
                let ring = if *style == Style::Primary {
                    theme::INK
                } else {
                    theme::ACCENT
                };
                canvas.outline(area, 8.0 * s, ring, 1.5 * s);
            }
            let size = px(18);
            let icon_top = (middle as i32) - size / 2;
            if *style == Style::Icon {
                if let Some(icon) = icon {
                    let left = (area.left + area.right - size) / 2;
                    icon.draw(canvas, left, icon_top, size, colour);
                }
                return;
            }
            let text = canvas.text_width(label, font);
            let total = text + if icon.is_some() { size + px(8) } else { 0 };
            let mut left = (area.left + area.right - total) / 2;
            if let Some(icon) = icon {
                icon.draw(canvas, left, icon_top, size, colour);
                left += size + px(8);
            }
            canvas.text(
                label,
                RECT {
                    left,
                    right: left + text + px(2),
                    ..area
                },
                font,
                colour,
                DT_LEFT | LINE,
            );
        }
    }
}

/// A secondary button's colour at rest: a step up from what is behind it.
fn rest(on_page: bool) -> u32 {
    if on_page {
        theme::SURFACE
    } else {
        theme::HOVER
    }
}
