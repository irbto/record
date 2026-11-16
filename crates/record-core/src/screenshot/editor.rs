//! Annotation state for the screenshot overlay.
//!
//! [`Editor`] owns the frozen desktop image, every committed annotation, the
//! selection, and the toolbar. It receives pointer and key input as plain
//! values and draws complete overlay frames into a [`Canvas`]. It has no
//! Windows dependency. The overlay window only translates messages and
//! presents the frames.

use super::{
    canvas::{Area, Canvas, Mask, Point, rgb},
    icons::{Icon, draw_icon},
};

/// Colors offered by the toolbar swatches and the number keys.
pub(crate) const PALETTE: [u32; 6] = [
    rgb(0xff, 0x3b, 0x30),
    rgb(0xff, 0xd6, 0x0a),
    rgb(0x34, 0xc7, 0x59),
    rgb(0x0a, 0x84, 0xff),
    rgb(0xff, 0xff, 0xff),
    rgb(0x00, 0x00, 0x00),
];

/// Largest value of the size setting.
const MAX_SIZE: u32 = 10;
/// Toolbar background color.
const BAR_COLOR: u32 = rgb(0x1b, 0x1e, 0x24);
/// Background of the button under the pointer.
const HOVER_COLOR: u32 = rgb(0x33, 0x38, 0x41);
/// Background of the active tool button.
const ACTIVE_COLOR: u32 = rgb(0x1f, 0x6f, 0xeb);
/// Color of the toolbar outline and its separators.
const BORDER_COLOR: u32 = rgb(0x3a, 0x3f, 0x47);
/// Toolbar label color.
const LABEL_COLOR: u32 = rgb(0xff, 0xff, 0xff);

/// Turns text into coverage masks with a platform font.
pub(crate) trait TextRasterizer {
    /// Renders one line of text at the given pixel height.
    fn rasterize(&self, text: &str, pixel_height: u32) -> Mask;
}

/// Identifies what a drag on the image does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tool {
    /// Selects the region that the screenshot keeps.
    Select,
    /// Draws a rectangle outline.
    Rectangle,
    /// Draws an ellipse outline.
    Ellipse,
    /// Draws a straight line.
    Line,
    /// Draws a line with an arrow head.
    Arrow,
    /// Draws a freehand stroke.
    Pen,
    /// Places typed text.
    Text,
    /// Tints a rectangle like a marker pen.
    Marker,
    /// Replaces a rectangle with coarse blocks.
    Pixelate,
    /// Blurs a rectangle.
    Blur,
    /// Places the next number in a filled circle.
    Step,
    /// Darkens everything outside a rectangle.
    Spotlight,
    /// Covers a rectangle with the color under the start of the drag.
    Eraser,
}

impl Tool {
    /// Every tool in toolbar order, with its name and shortcut key.
    const ALL: [(Self, &'static str, char); 13] = [
        (Self::Select, "Select region", 's'),
        (Self::Rectangle, "Rectangle", 'r'),
        (Self::Ellipse, "Ellipse", 'e'),
        (Self::Line, "Line", 'l'),
        (Self::Arrow, "Arrow", 'a'),
        (Self::Pen, "Pen", 'p'),
        (Self::Text, "Text", 't'),
        (Self::Marker, "Marker", 'h'),
        (Self::Pixelate, "Pixelate", 'x'),
        (Self::Blur, "Blur", 'b'),
        (Self::Step, "Step number", 'n'),
        (Self::Spotlight, "Spotlight", 'o'),
        (Self::Eraser, "Eraser", 'd'),
    ];
}

/// One key press after the window translated it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Key {
    /// A printable character.
    Char(char),
    /// The Enter key.
    Enter,
    /// The Escape key.
    Escape,
    /// The Backspace key.
    Backspace,
    /// The Tab key.
    Tab,
    /// An arrow key. `resize` is set while Shift is down.
    Arrow {
        /// Horizontal direction: -1, 0, or 1.
        dx: i32,
        /// Vertical direction: -1, 0, or 1.
        dy: i32,
        /// Changes the selection size instead of its position.
        resize: bool,
    },
    /// Ctrl+Z.
    Undo,
    /// Ctrl+Y.
    Redo,
    /// Ctrl+C.
    Copy,
    /// Ctrl+S.
    Save,
}

/// Tells the window what to do after one input event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    /// Keep the overlay open.
    Continue,
    /// Close the overlay and write the PNG file.
    Save,
    /// Close the overlay and only copy the image to the clipboard.
    CopyOnly,
    /// Close the overlay and discard the screenshot.
    Cancel,
}

/// The drawable content of one annotation, in image pixels.
#[derive(Clone, Debug, PartialEq)]
enum Shape {
    Rectangle(Area),
    Ellipse(Area),
    Line(Point, Point),
    Arrow(Point, Point),
    Pen(Vec<Point>),
    Marker(Area),
    Pixelate(Area),
    Blur(Area),
    Text(Point, Mask),
    Step(Point, Mask),
    Spotlight(Area),
    Erase(Area),
}

/// One committed drawing step with the settings it was made with.
#[derive(Clone, Debug, PartialEq)]
struct Annotation {
    shape: Shape,
    color: u32,
    /// Stroke width, block size, or blur radius in pixels.
    weight: i32,
}

impl Annotation {
    /// Draws this annotation into an image.
    fn draw(&self, canvas: &mut Canvas) {
        let stroke = self.weight as f32;
        match &self.shape {
            Shape::Rectangle(area) => canvas.stroke_rect(*area, self.weight, self.color),
            Shape::Ellipse(area) => canvas.stroke_ellipse(*area, stroke, self.color),
            Shape::Line(from, to) => canvas.stroke_line(*from, *to, stroke, self.color),
            Shape::Arrow(from, to) => canvas.stroke_arrow(*from, *to, stroke, self.color),
            Shape::Pen(points) => {
                if let [only] = points.as_slice() {
                    canvas.stroke_line(*only, *only, stroke, self.color);
                }
                for pair in points.windows(2) {
                    canvas.stroke_line(pair[0], pair[1], stroke, self.color);
                }
            }
            Shape::Marker(area) => canvas.highlight(*area, self.color),
            Shape::Pixelate(area) => canvas.pixelate(*area, self.weight),
            Shape::Blur(area) => canvas.blur(*area, self.weight),
            Shape::Text(origin, mask) => canvas.draw_mask(*origin, mask, self.color),
            Shape::Step(center, mask) => {
                canvas.stroke_line(*center, *center, stroke * 2.0, self.color);
                let (red, green, blue) = (
                    (self.color >> 16) & 0xff,
                    (self.color >> 8) & 0xff,
                    self.color & 0xff,
                );
                let bright = red * 3 + green * 6 + blue > 1_400;
                let digits = if bright { 0 } else { LABEL_COLOR };
                let origin = Point::new(
                    center.x - mask.width as i32 / 2,
                    center.y - mask.height as i32 / 2,
                );
                canvas.draw_mask(origin, mask, digits);
            }
            Shape::Spotlight(area) => canvas.dim_outside(*area),
            Shape::Erase(area) => canvas.fill_rect(*area, self.color, 255),
        }
    }
}

/// The pointer interaction in progress.
#[derive(Clone, Debug, PartialEq)]
enum Drag {
    /// No button is down.
    Idle,
    /// A new selection grows from the anchor.
    Selecting { anchor: Point, current: Point },
    /// The selection follows the pointer.
    Moving { grab: Point, original: Area },
    /// A shape grows from the anchor. The pen records its complete path.
    Drawing {
        anchor: Point,
        current: Point,
        path: Vec<Point>,
    },
}

/// Text that the user is typing but has not committed.
#[derive(Clone, Debug, PartialEq)]
struct TextEntry {
    origin: Point,
    content: String,
    mask: Mask,
    pixel_height: u32,
}

/// What one toolbar button does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Tool(Tool),
    Color(usize),
    SizeDown,
    SizeUp,
    Undo,
    Redo,
    Copy,
    Save,
    Close,
}

impl Action {
    /// Returns the icon of the button. Color buttons draw a swatch instead.
    const fn icon(self) -> Option<Icon> {
        Some(match self {
            Self::Tool(Tool::Select) => Icon::Select,
            Self::Tool(Tool::Rectangle) => Icon::Rectangle,
            Self::Tool(Tool::Ellipse) => Icon::Ellipse,
            Self::Tool(Tool::Line) => Icon::Line,
            Self::Tool(Tool::Arrow) => Icon::Arrow,
            Self::Tool(Tool::Pen) => Icon::Pen,
            Self::Tool(Tool::Text) => Icon::Text,
            Self::Tool(Tool::Marker) => Icon::Marker,
            Self::Tool(Tool::Pixelate) => Icon::Pixelate,
            Self::Tool(Tool::Blur) => Icon::Blur,
            Self::Tool(Tool::Step) => Icon::Step,
            Self::Tool(Tool::Spotlight) => Icon::Spotlight,
            Self::Tool(Tool::Eraser) => Icon::Eraser,
            Self::SizeDown => Icon::Minus,
            Self::SizeUp => Icon::Plus,
            Self::Undo => Icon::Undo,
            Self::Redo => Icon::Redo,
            Self::Copy => Icon::Copy,
            Self::Save => Icon::Save,
            Self::Close => Icon::Close,
            Self::Color(_) => return None,
        })
    }
}

/// One toolbar button with its position relative to the toolbar corner.
#[derive(Clone, Debug)]
struct Button {
    action: Action,
    left: i32,
    /// The name and shortcut shown while the pointer rests on the button.
    tip: Mask,
}

/// The floating tool strip. It is part of the overlay, never of the image.
#[derive(Clone, Debug)]
struct Toolbar {
    buttons: Vec<Button>,
    /// Positions of the thin lines between button groups.
    separators: Vec<i32>,
    /// Left edge of the stroke size preview.
    size_left: i32,
    /// Side of one square button.
    button: i32,
    width: i32,
    height: i32,
    margin: i32,
}

impl Toolbar {
    /// Places the buttons in one row and renders their tips.
    ///
    /// The buttons shrink when the row is wider than `limit` pixels.
    fn build(text: &dyn TextRasterizer, scale: f32, limit: i32) -> Self {
        let px = |value: f32| (value * scale).round().max(1.0) as i32;
        let (pad, gap) = (px(5.0), px(9.0));
        let count = (Tool::ALL.len() + PALETTE.len() + 8) as i32;
        let fitting = (limit - 3 * gap - 2 * pad) / count;
        let button = px(36.0).min(fitting).max(px(18.0));
        let font = px(14.0) as u32;
        let mut toolbar = Self {
            buttons: Vec::new(),
            separators: Vec::new(),
            size_left: 0,
            button,
            width: 0,
            height: button + 2 * pad,
            margin: px(12.0),
        };
        let mut cursor = pad;
        let add = |toolbar: &mut Self, cursor: &mut i32, action: Action, tip: String| {
            toolbar.buttons.push(Button {
                action,
                left: *cursor,
                tip: text.rasterize(&tip, font),
            });
            *cursor += button;
        };
        let separate = |toolbar: &mut Self, cursor: &mut i32| {
            toolbar.separators.push(*cursor + gap / 2);
            *cursor += gap;
        };
        for (tool, name, key) in Tool::ALL {
            let tip = format!("{name}  {}", key.to_ascii_uppercase());
            add(&mut toolbar, &mut cursor, Action::Tool(tool), tip);
        }
        separate(&mut toolbar, &mut cursor);
        for index in 0..PALETTE.len() {
            let tip = format!("Color  {}", index + 1);
            add(&mut toolbar, &mut cursor, Action::Color(index), tip);
        }
        separate(&mut toolbar, &mut cursor);
        add(
            &mut toolbar,
            &mut cursor,
            Action::SizeDown,
            "Smaller  [".to_owned(),
        );
        toolbar.size_left = cursor;
        cursor += button;
        add(
            &mut toolbar,
            &mut cursor,
            Action::SizeUp,
            "Larger  ]".to_owned(),
        );
        separate(&mut toolbar, &mut cursor);
        for (action, tip) in [
            (Action::Undo, "Undo  Ctrl+Z"),
            (Action::Redo, "Redo  Ctrl+Y"),
            (Action::Copy, "Copy only  Ctrl+C"),
            (Action::Save, "Save  Enter"),
            (Action::Close, "Close  Esc"),
        ] {
            add(&mut toolbar, &mut cursor, action, tip.to_owned());
        }
        toolbar.width = cursor + pad;
        toolbar
    }

    /// Returns the toolbar rectangle on a screen of the given size.
    fn area(&self, screen: Area, bottom: bool) -> Area {
        let left = (screen.left + (screen.width() - self.width) / 2).max(screen.left);
        let top = if bottom {
            screen.bottom - self.margin - self.height
        } else {
            screen.top + self.margin
        };
        Area::new(left, top, left + self.width, top + self.height)
    }

    /// Returns the button rectangle in screen pixels.
    fn button_area(&self, bar: Area, button: &Button) -> Area {
        let top = bar.top + (self.height - self.button) / 2;
        Area::new(
            bar.left + button.left,
            top,
            bar.left + button.left + self.button,
            top + self.button,
        )
    }

    /// Finds the action of the button under the pointer.
    fn hit(&self, bar: Area, point: Point) -> Option<Action> {
        self.buttons
            .iter()
            .find(|button| self.button_area(bar, button).contains(point))
            .map(|button| button.action)
    }
}

/// Complete state of one screenshot editing session.
pub(crate) struct Editor {
    base: Canvas,
    flattened: Canvas,
    annotations: Vec<Annotation>,
    redo: Vec<Annotation>,
    tool: Tool,
    color: usize,
    size: u32,
    scale: f32,
    selection: Option<Area>,
    selection_label: Option<(Area, Mask)>,
    drag: Drag,
    text_entry: Option<TextEntry>,
    pointer: Point,
    toolbar: Toolbar,
    toolbar_bottom: bool,
    text: Box<dyn TextRasterizer>,
}

impl Editor {
    /// Starts an editing session on a captured image.
    ///
    /// `scale` is the display scale of the monitor, where 1.0 is 96 DPI.
    pub(crate) fn new(base: Canvas, scale: f32, text: Box<dyn TextRasterizer>) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(0.5, 8.0)
        } else {
            1.0
        };
        let toolbar = Toolbar::build(text.as_ref(), scale, base.width() as i32);
        Self {
            flattened: base.clone(),
            base,
            annotations: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Select,
            color: 0,
            size: 3,
            scale,
            selection: None,
            selection_label: None,
            drag: Drag::Idle,
            text_entry: None,
            pointer: Point::new(-1, -1),
            toolbar,
            toolbar_bottom: false,
            text,
        }
    }

    /// Scales a length from 96 DPI units to image pixels.
    fn px(&self, value: f32) -> i32 {
        (value * self.scale).round().max(1.0) as i32
    }

    /// Returns the pixel weight that the current size gives a shape.
    fn weight(&self, tool: Tool) -> i32 {
        let size = self.size as f32;
        match tool {
            Tool::Pixelate => self.px(4.0 + 4.0 * size),
            Tool::Blur => self.px(2.0 + 2.0 * size),
            Tool::Text => self.px(10.0 + 6.0 * size),
            Tool::Step => self.px(8.0 + 2.0 * size),
            _ => self.px(2.0 * size - 1.0),
        }
    }

    /// Returns the current toolbar rectangle.
    fn toolbar_area(&self) -> Area {
        self.toolbar.area(self.base.bounds(), self.toolbar_bottom)
    }

    /// Handles a primary button press.
    pub(crate) fn pointer_down(&mut self, point: Point) -> Outcome {
        self.pointer = point;
        if let Some(action) = self.toolbar.hit(self.toolbar_area(), point) {
            return self.run(action);
        }
        if self.toolbar_area().contains(point) {
            return Outcome::Continue;
        }
        self.commit_text();
        self.drag = match self.tool {
            Tool::Select => match self.selection {
                Some(selection) if selection.contains(point) => Drag::Moving {
                    grab: point,
                    original: selection,
                },
                _ => Drag::Selecting {
                    anchor: point,
                    current: point,
                },
            },
            Tool::Text => {
                let pixel_height = self.weight(Tool::Text) as u32;
                self.text_entry = Some(TextEntry {
                    origin: Point::new(point.x, point.y - pixel_height as i32 / 2),
                    content: String::new(),
                    mask: Mask::default(),
                    pixel_height,
                });
                Drag::Idle
            }
            Tool::Step => {
                let radius = self.weight(Tool::Step);
                let number = 1 + self
                    .annotations
                    .iter()
                    .filter(|annotation| matches!(annotation.shape, Shape::Step(..)))
                    .count();
                let mask = self
                    .text
                    .rasterize(&number.to_string(), (radius * 5 / 4) as u32);
                self.push(Annotation {
                    shape: Shape::Step(point, mask),
                    color: PALETTE[self.color],
                    weight: radius,
                });
                Drag::Idle
            }
            _ => Drag::Drawing {
                anchor: point,
                current: point,
                path: vec![point],
            },
        };
        Outcome::Continue
    }

    /// Handles pointer motion with or without a pressed button.
    pub(crate) fn pointer_move(&mut self, point: Point) {
        self.pointer = point;
        match &mut self.drag {
            Drag::Idle => {}
            Drag::Selecting { current, .. } => *current = point,
            Drag::Moving { grab, original } => {
                let bounds = self.base.bounds();
                let dx = (point.x - grab.x).clamp(-original.left, bounds.right - original.right);
                let dy = (point.y - grab.y).clamp(-original.top, bounds.bottom - original.bottom);
                let moved = original.offset(dx, dy);
                self.set_selection(Some(moved));
            }
            Drag::Drawing { current, path, .. } => {
                *current = point;
                if path.last() != Some(&point) {
                    path.push(point);
                }
            }
        }
    }

    /// Handles a primary button release.
    pub(crate) fn pointer_up(&mut self, point: Point) {
        self.pointer_move(point);
        match std::mem::replace(&mut self.drag, Drag::Idle) {
            Drag::Selecting { anchor, current } => {
                let area = Area::from_points(anchor, current).intersect(&self.base.bounds());
                let selection = (area.width() >= 3 && area.height() >= 3).then_some(area);
                self.set_selection(selection);
            }
            drag @ Drag::Drawing { .. } => {
                if let Some(annotation) = self.annotation_for(&drag) {
                    self.push(annotation);
                }
            }
            Drag::Idle | Drag::Moving { .. } => {}
        }
    }

    /// Changes the size setting by wheel notches. Positive notches enlarge.
    pub(crate) fn wheel(&mut self, notches: i32) {
        self.size = (self.size as i32 + notches).clamp(1, MAX_SIZE as i32) as u32;
    }

    /// Handles one translated key press.
    pub(crate) fn key(&mut self, key: Key) -> Outcome {
        if self.text_entry.is_some() {
            match key {
                Key::Char(character) => self.edit_text(|content| content.push(character)),
                Key::Backspace => self.edit_text(|content| {
                    content.pop();
                }),
                Key::Enter => self.commit_text(),
                Key::Escape => self.text_entry = None,
                Key::Save | Key::Copy | Key::Undo | Key::Redo => {
                    self.commit_text();
                    return self.key(key);
                }
                Key::Tab | Key::Arrow { .. } => {}
            }
            return Outcome::Continue;
        }
        match key {
            Key::Enter | Key::Save => return Outcome::Save,
            Key::Copy => return Outcome::CopyOnly,
            Key::Escape => {
                if self.drag == Drag::Idle {
                    return Outcome::Cancel;
                }
                self.drag = Drag::Idle;
            }
            Key::Undo => self.undo(),
            Key::Redo => self.redo(),
            Key::Tab => self.toolbar_bottom = !self.toolbar_bottom,
            Key::Backspace => {}
            Key::Arrow { dx, dy, resize } => self.nudge_selection(dx, dy, resize),
            Key::Char(character) => self.shortcut(character.to_ascii_lowercase()),
        }
        Outcome::Continue
    }

    /// Applies a single-character shortcut.
    fn shortcut(&mut self, character: char) {
        if let Some((tool, _, _)) = Tool::ALL.iter().find(|(_, _, key)| *key == character) {
            self.tool = *tool;
        } else if let Some(index) = character.to_digit(10).filter(|digit| *digit >= 1) {
            if (index as usize) <= PALETTE.len() {
                self.color = index as usize - 1;
            }
        } else if character == '[' {
            self.wheel(-1);
        } else if character == ']' {
            self.wheel(1);
        }
    }

    /// Runs a toolbar button.
    fn run(&mut self, action: Action) -> Outcome {
        self.commit_text();
        match action {
            Action::Tool(tool) => self.tool = tool,
            Action::Color(index) => self.color = index,
            Action::SizeDown => self.wheel(-1),
            Action::SizeUp => self.wheel(1),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Copy => return Outcome::CopyOnly,
            Action::Save => return Outcome::Save,
            Action::Close => return Outcome::Cancel,
        }
        Outcome::Continue
    }

    /// Changes the pending text and renders it again.
    fn edit_text(&mut self, change: impl FnOnce(&mut String)) {
        if let Some(entry) = &mut self.text_entry {
            change(&mut entry.content);
            entry.mask = if entry.content.is_empty() {
                Mask::default()
            } else {
                self.text.rasterize(&entry.content, entry.pixel_height)
            };
        }
    }

    /// Turns pending text into an annotation. Empty text leaves no trace.
    fn commit_text(&mut self) {
        if let Some(entry) = self.text_entry.take()
            && entry.mask.width > 0
        {
            self.push(Annotation {
                shape: Shape::Text(entry.origin, entry.mask),
                color: PALETTE[self.color],
                weight: entry.pixel_height as i32,
            });
        }
    }

    /// Builds the annotation that a drawing drag represents right now.
    fn annotation_for(&self, drag: &Drag) -> Option<Annotation> {
        let Drag::Drawing {
            anchor,
            current,
            path,
        } = drag
        else {
            return None;
        };
        let area = Area::from_points(*anchor, *current);
        let moved = anchor != current;
        let large = area.width() >= 3 && area.height() >= 3;
        let shape = match self.tool {
            Tool::Rectangle if large => Shape::Rectangle(area),
            Tool::Ellipse if large => Shape::Ellipse(area),
            Tool::Marker if large => Shape::Marker(area),
            Tool::Pixelate if large => Shape::Pixelate(area),
            Tool::Blur if large => Shape::Blur(area),
            Tool::Spotlight if large => Shape::Spotlight(area),
            Tool::Eraser if large => {
                return Some(Annotation {
                    shape: Shape::Erase(area),
                    color: self.flattened.pixel(anchor.x, anchor.y).unwrap_or(0),
                    weight: 0,
                });
            }
            Tool::Line if moved => Shape::Line(*anchor, *current),
            Tool::Arrow if moved => Shape::Arrow(*anchor, *current),
            Tool::Pen => Shape::Pen(path.clone()),
            _ => return None,
        };
        Some(Annotation {
            shape,
            color: PALETTE[self.color],
            weight: self.weight(self.tool),
        })
    }

    /// Commits one annotation and draws it into the cached image.
    fn push(&mut self, annotation: Annotation) {
        annotation.draw(&mut self.flattened);
        self.annotations.push(annotation);
        self.redo.clear();
    }

    /// Removes the newest annotation.
    fn undo(&mut self) {
        if let Some(annotation) = self.annotations.pop() {
            self.redo.push(annotation);
            self.flattened.copy_from(&self.base);
            for annotation in &self.annotations {
                annotation.draw(&mut self.flattened);
            }
        }
    }

    /// Restores the annotation that the last undo removed.
    fn redo(&mut self) {
        if let Some(annotation) = self.redo.pop() {
            annotation.draw(&mut self.flattened);
            self.annotations.push(annotation);
        }
    }

    /// Moves or resizes the selection by one pixel.
    fn nudge_selection(&mut self, dx: i32, dy: i32, resize: bool) {
        let Some(selection) = self.selection else {
            return;
        };
        let bounds = self.base.bounds();
        let changed = if resize {
            Area::new(
                selection.left,
                selection.top,
                (selection.right + dx).max(selection.left + 3),
                (selection.bottom + dy).max(selection.top + 3),
            )
            .intersect(&bounds)
        } else {
            let dx = dx.clamp(-selection.left, bounds.right - selection.right);
            let dy = dy.clamp(-selection.top, bounds.bottom - selection.bottom);
            selection.offset(dx, dy)
        };
        self.set_selection(Some(changed));
    }

    /// Stores the selection and refreshes its size label when the size changes.
    pub(crate) fn set_selection(&mut self, selection: Option<Area>) {
        self.selection = selection;
        self.refresh_selection_label(selection);
    }

    /// Renders a `WIDTH x HEIGHT` label for the given area when it is stale.
    fn refresh_selection_label(&mut self, area: Option<Area>) {
        let Some(area) = area else {
            self.selection_label = None;
            return;
        };
        let stale = self.selection_label.as_ref().is_none_or(|(labeled, _)| {
            labeled.width() != area.width() || labeled.height() != area.height()
        });
        if stale {
            let label = format!("{} x {}", area.width(), area.height());
            let mask = self.text.rasterize(&label, self.px(14.0) as u32);
            self.selection_label = Some((area, mask));
        }
    }

    /// Returns the selection that the overlay shows, including a live drag.
    fn visible_selection(&self) -> Option<Area> {
        match &self.drag {
            Drag::Selecting { anchor, current } => {
                Some(Area::from_points(*anchor, *current).intersect(&self.base.bounds()))
            }
            _ => self.selection,
        }
    }

    /// Draws one complete overlay frame: image, live shape, selection, toolbar.
    pub(crate) fn compose(&mut self, frame: &mut Canvas) {
        frame.copy_from(&self.flattened);
        if let Some(annotation) = self.annotation_for(&self.drag) {
            annotation.draw(frame);
        }
        if let Some(entry) = &self.text_entry {
            let color = PALETTE[self.color];
            frame.draw_mask(entry.origin, &entry.mask, color);
            let caret_x = entry.origin.x + entry.mask.width as i32 + self.px(1.0);
            frame.fill_rect(
                Area::new(
                    caret_x,
                    entry.origin.y,
                    caret_x + self.px(2.0),
                    entry.origin.y + entry.pixel_height as i32,
                ),
                color,
                255,
            );
        }
        let selection = self.visible_selection();
        if let Some(selection) = selection.filter(|selection| !selection.is_empty()) {
            self.refresh_selection_label(Some(selection));
            frame.dim_outside(selection);
            frame.stroke_rect(selection.inflate(2), 1, 0);
            frame.stroke_rect(selection.inflate(1), 1, LABEL_COLOR);
            if let Some((_, mask)) = &self.selection_label {
                let pad = self.px(4.0);
                let height = mask.height as i32 + 2 * pad;
                let top = if selection.top - height - pad >= 0 {
                    selection.top - height - pad
                } else {
                    selection.top + pad
                };
                let plate = Area::new(
                    selection.left,
                    top,
                    selection.left + mask.width as i32 + 2 * pad,
                    top + height,
                );
                frame.fill_rect(plate, BAR_COLOR, 230);
                frame.draw_mask(Point::new(plate.left + pad, top + pad), mask, LABEL_COLOR);
            }
        }
        self.draw_toolbar(frame);
    }

    /// Draws the toolbar: icons, colors, the size preview, and the tip of
    /// the button under the pointer.
    fn draw_toolbar(&self, frame: &mut Canvas) {
        let bar = self.toolbar_area();
        let corner = self.px(10.0) as f32;
        let side = self.toolbar.button;
        // A soft shadow from a few wide, faint layers.
        for layer in 1..=4 {
            let spread = self.px(layer as f32 * 1.5);
            frame.fill_round_rect(
                bar.offset(0, self.px(2.0)).inflate(spread),
                corner + spread as f32,
                0,
                16,
            );
        }
        frame.fill_round_rect(bar.inflate(1), corner + 1.0, BORDER_COLOR, 255);
        frame.fill_round_rect(bar, corner, BAR_COLOR, 255);
        for separator in &self.toolbar.separators {
            let x = bar.left + separator;
            frame.fill_rect(
                Area::new(
                    x,
                    bar.top + self.px(10.0),
                    x + 1,
                    bar.bottom - self.px(10.0),
                ),
                BORDER_COLOR,
                255,
            );
        }
        let mut hovered_button = None;
        for button in &self.toolbar.buttons {
            let area = self.toolbar.button_area(bar, button);
            let plate = area.inflate(-self.px(2.0));
            let active = button.action == Action::Tool(self.tool);
            let hovered = self.drag == Drag::Idle && area.contains(self.pointer);
            if hovered {
                hovered_button = Some(button);
            }
            let paper = if active {
                ACTIVE_COLOR
            } else if hovered {
                HOVER_COLOR
            } else {
                BAR_COLOR
            };
            if paper != BAR_COLOR {
                frame.fill_round_rect(plate, self.px(7.0) as f32, paper, 255);
            }
            if let Action::Color(index) = button.action {
                let dot = area.inflate(-(side * 5 / 18));
                if index == self.color {
                    frame.fill_round_rect(dot.inflate(self.px(4.0)), side as f32, LABEL_COLOR, 255);
                    frame.fill_round_rect(dot.inflate(self.px(2.0)), side as f32, paper, 255);
                }
                frame.fill_round_rect(dot.inflate(1), side as f32, BORDER_COLOR, 255);
                frame.fill_round_rect(dot, side as f32, PALETTE[index], 255);
            } else if let Some(icon) = button.action.icon() {
                draw_icon(frame, icon, area.inflate(-(side / 4)), LABEL_COLOR, paper);
            }
        }
        // The size preview is a dot as wide as the stroke it stands for.
        let slot = Area::new(
            bar.left + self.toolbar.size_left,
            bar.top,
            bar.left + self.toolbar.size_left + side,
            bar.bottom,
        );
        let diameter = self.weight(Tool::Line).clamp(2, (side * 3 / 4).max(2));
        let left = slot.left + (side - diameter) / 2;
        let top = slot.top + (slot.height() - diameter) / 2;
        frame.fill_round_rect(
            Area::new(left, top, left + diameter, top + diameter),
            side as f32,
            PALETTE[self.color],
            255,
        );
        if let Some(button) = hovered_button {
            let pad = self.px(7.0);
            let area = self.toolbar.button_area(bar, button);
            let width = button.tip.width as i32 + 2 * pad;
            let height = button.tip.height as i32 + pad;
            let screen = self.base.bounds();
            let left = (area.left + (side - width) / 2)
                .clamp(screen.left, (screen.right - width).max(screen.left));
            let top = if self.toolbar_bottom {
                bar.top - height - self.px(6.0)
            } else {
                bar.bottom + self.px(6.0)
            };
            let plate = Area::new(left, top, left + width, top + height);
            frame.fill_round_rect(
                plate.inflate(1),
                self.px(6.0) as f32 + 1.0,
                BORDER_COLOR,
                255,
            );
            frame.fill_round_rect(plate, self.px(6.0) as f32, BAR_COLOR, 255);
            frame.draw_mask(centered(plate, &button.tip), &button.tip, LABEL_COLOR);
        }
    }

    /// Returns the committed selection in image pixels.
    #[must_use]
    pub(crate) const fn selection(&self) -> Option<Area> {
        self.selection
    }

    /// Returns the final image: all annotations, cropped to the selection.
    #[must_use]
    pub(crate) fn result(&self) -> Canvas {
        match self.selection {
            Some(selection) => self.flattened.crop(selection),
            None => self.flattened.clone(),
        }
    }
}

/// Returns the corner that centers a mask inside an area.
fn centered(area: Area, mask: &Mask) -> Point {
    Point::new(
        area.left + (area.width() - mask.width as i32) / 2,
        area.top + (area.height() - mask.height as i32) / 2,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Renders every character as a solid box, 0.6 em wide.
    pub(crate) struct BoxText;

    impl TextRasterizer for BoxText {
        fn rasterize(&self, text: &str, pixel_height: u32) -> Mask {
            let advance = (pixel_height * 3 / 5).max(1);
            let width = advance * text.chars().count() as u32;
            let mut coverage = vec![0; (width * pixel_height) as usize];
            for y in 2..pixel_height.saturating_sub(2) {
                for x in 0..width {
                    if x % advance != advance - 1 {
                        coverage[(y * width + x) as usize] = 255;
                    }
                }
            }
            Mask {
                width,
                height: pixel_height,
                coverage,
            }
        }
    }

    const GRAY: u32 = 0x0080_8080;

    /// Creates an editor on a flat gray 800x600 image.
    fn editor() -> Editor {
        Editor::new(Canvas::new(800, 600, GRAY), 1.0, Box::new(BoxText))
    }

    /// Presses, moves, and releases the primary button.
    fn drag(editor: &mut Editor, from: (i32, i32), to: (i32, i32)) {
        assert_eq!(
            editor.pointer_down(Point::new(from.0, from.1)),
            Outcome::Continue
        );
        editor.pointer_move(Point::new((from.0 + to.0) / 2, (from.1 + to.1) / 2));
        editor.pointer_up(Point::new(to.0, to.1));
    }

    #[test]
    fn selection_crops_the_result() {
        let mut editor = editor();
        drag(&mut editor, (300, 400), (100, 200));
        assert_eq!(editor.selection, Some(Area::new(100, 200, 301, 401)));
        let result = editor.result();
        assert_eq!((result.width(), result.height()), (201, 201));
    }

    #[test]
    fn click_without_drag_clears_the_selection() {
        let mut editor = editor();
        drag(&mut editor, (100, 200), (300, 400));
        drag(&mut editor, (500, 500), (500, 500));
        assert_eq!(editor.selection, None);
        assert_eq!(editor.result().width(), 800);
    }

    #[test]
    fn dragging_inside_the_selection_moves_it_and_stays_on_screen() {
        let mut editor = editor();
        drag(&mut editor, (100, 200), (299, 399));
        drag(&mut editor, (150, 250), (160, 240));
        assert_eq!(editor.selection, Some(Area::new(110, 190, 310, 390)));
        drag(&mut editor, (150, 250), (-900, -900));
        assert_eq!(editor.selection, Some(Area::new(0, 0, 200, 200)));
    }

    #[test]
    fn arrow_keys_nudge_and_resize_the_selection() {
        let mut editor = editor();
        drag(&mut editor, (100, 200), (299, 399));
        let step = |dx, dy, resize| Key::Arrow { dx, dy, resize };
        editor.key(step(1, 0, false));
        editor.key(step(0, -1, true));
        assert_eq!(editor.selection, Some(Area::new(101, 200, 301, 399)));
    }

    #[test]
    fn shapes_draw_undo_and_redo() {
        let mut editor = editor();
        assert_eq!(editor.key(Key::Char('r')), Outcome::Continue);
        drag(&mut editor, (100, 200), (300, 400));
        let red = PALETTE[0];
        assert_eq!(editor.result().pixel(100, 300), Some(red));
        assert_eq!(editor.result().pixel(200, 300), Some(GRAY));
        editor.key(Key::Undo);
        assert_eq!(editor.result().pixel(100, 300), Some(GRAY));
        editor.key(Key::Redo);
        assert_eq!(editor.result().pixel(100, 300), Some(red));
    }

    #[test]
    fn a_new_shape_discards_the_redo_history() {
        let mut editor = editor();
        editor.key(Key::Char('l'));
        drag(&mut editor, (100, 200), (300, 200));
        editor.key(Key::Undo);
        drag(&mut editor, (100, 300), (300, 300));
        editor.key(Key::Redo);
        assert_eq!(editor.annotations.len(), 1);
        assert_eq!(editor.result().pixel(200, 200), Some(GRAY));
    }

    #[test]
    fn the_live_frame_shows_an_unfinished_shape_but_the_result_does_not() {
        let mut editor = editor();
        editor.key(Key::Char('l'));
        editor.pointer_down(Point::new(100, 300));
        editor.pointer_move(Point::new(300, 300));
        let mut frame = Canvas::new(800, 600, 0);
        editor.compose(&mut frame);
        assert_eq!(frame.pixel(200, 300), Some(PALETTE[0]));
        assert_eq!(editor.result().pixel(200, 300), Some(GRAY));
        assert_eq!(editor.key(Key::Escape), Outcome::Continue);
        editor.pointer_up(Point::new(300, 300));
        assert!(editor.annotations.is_empty());
        assert_eq!(editor.key(Key::Escape), Outcome::Cancel);
    }

    #[test]
    fn the_toolbar_and_dimming_never_reach_the_result() {
        let mut editor = editor();
        drag(&mut editor, (100, 200), (300, 400));
        let mut frame = Canvas::new(800, 600, 0);
        editor.compose(&mut frame);
        let bar = editor.toolbar_area();
        assert_eq!(
            frame.pixel(bar.right - 2, bar.top + bar.height() / 2),
            Some(BAR_COLOR)
        );
        assert_ne!(frame.pixel(700, 500), Some(GRAY));
        assert_eq!(frame.pixel(200, 300), Some(GRAY));
        assert!(editor.flattened.pixels().iter().all(|pixel| *pixel == GRAY));
    }

    #[test]
    fn toolbar_buttons_select_tools_colors_and_finish() {
        let mut editor = Editor::new(Canvas::new(1600, 900, GRAY), 1.0, Box::new(BoxText));
        let bar = editor.toolbar_area();
        let center = |editor: &Editor, wanted: Action| {
            let button = editor
                .toolbar
                .buttons
                .iter()
                .find(|button| button.action == wanted)
                .unwrap();
            let area = editor.toolbar.button_area(bar, button);
            Point::new(area.left + area.width() / 2, area.top + area.height() / 2)
        };
        let blur = center(&editor, Action::Tool(Tool::Blur));
        assert_eq!(editor.pointer_down(blur), Outcome::Continue);
        editor.pointer_up(blur);
        assert_eq!(editor.tool, Tool::Blur);
        assert_eq!(editor.drag, Drag::Idle);
        let blue = center(&editor, Action::Color(3));
        editor.pointer_down(blue);
        assert_eq!(editor.color, 3);
        let larger = center(&editor, Action::SizeUp);
        editor.pointer_down(larger);
        assert_eq!(editor.size, 4);
        let save = center(&editor, Action::Save);
        assert_eq!(editor.pointer_down(save), Outcome::Save);
        let close = center(&editor, Action::Close);
        assert_eq!(editor.pointer_down(close), Outcome::Cancel);
        assert!(bar.right <= 1600, "toolbar is {} pixels wide", bar.width());
    }

    #[test]
    fn the_toolbar_shrinks_to_fit_a_narrow_screen_and_shows_tips() {
        let mut editor = editor();
        let bar = editor.toolbar_area();
        assert!(bar.left >= 0 && bar.right <= 800, "{bar:?}");
        let wide = Editor::new(Canvas::new(2560, 1440, GRAY), 1.0, Box::new(BoxText));
        assert_eq!(wide.toolbar.button, 36);
        assert!(editor.toolbar.button < 36);
        let blur = editor
            .toolbar
            .buttons
            .iter()
            .find(|button| button.action == Action::Tool(Tool::Blur))
            .unwrap();
        let area = editor.toolbar.button_area(bar, blur);
        let mut plain = Canvas::new(800, 600, 0);
        editor.compose(&mut plain);
        editor.pointer_move(Point::new(area.left + 3, area.top + 3));
        let mut hovered = Canvas::new(800, 600, 0);
        editor.compose(&mut hovered);
        let below = |frame: &Canvas| {
            (0..800)
                .filter(|x| frame.pixel(*x, bar.bottom + 14) != Some(GRAY))
                .count()
        };
        assert_eq!(below(&plain), 0);
        assert!(below(&hovered) > 20, "the tip is missing");
    }

    #[test]
    fn tab_moves_the_toolbar_to_the_other_edge() {
        let mut editor = editor();
        let top = editor.toolbar_area();
        editor.key(Key::Tab);
        let bottom = editor.toolbar_area();
        assert!(top.top < 100 && bottom.bottom > 500);
    }

    #[test]
    fn typed_text_becomes_one_annotation_and_keeps_shortcuts_out() {
        let mut editor = editor();
        editor.key(Key::Char('t'));
        editor.pointer_down(Point::new(100, 300));
        editor.pointer_up(Point::new(100, 300));
        for character in "bxs".chars() {
            editor.key(Key::Char(character));
        }
        assert_eq!(editor.tool, Tool::Text);
        editor.key(Key::Backspace);
        assert_eq!(editor.key(Key::Enter), Outcome::Continue);
        assert_eq!(editor.annotations.len(), 1);
        assert_eq!(editor.result().pixel(102, 300), Some(PALETTE[0]));
        assert_eq!(editor.key(Key::Enter), Outcome::Save);
    }

    #[test]
    fn empty_or_cancelled_text_leaves_no_annotation() {
        let mut editor = editor();
        editor.key(Key::Char('t'));
        editor.pointer_down(Point::new(100, 300));
        editor.key(Key::Enter);
        editor.pointer_down(Point::new(100, 300));
        editor.key(Key::Char('a'));
        assert_eq!(editor.key(Key::Escape), Outcome::Continue);
        assert!(editor.annotations.is_empty());
    }

    #[test]
    fn saving_while_typing_keeps_the_text() {
        let mut editor = editor();
        editor.key(Key::Char('t'));
        editor.pointer_down(Point::new(100, 300));
        editor.key(Key::Char('a'));
        assert_eq!(editor.key(Key::Save), Outcome::Save);
        assert_eq!(editor.annotations.len(), 1);
    }

    #[test]
    fn redaction_tools_change_only_their_area() {
        let mut base = Canvas::new(800, 600, 0);
        base.fill_rect(Area::new(0, 0, 400, 600), 0x00ff_ffff, 255);
        let mut editor = Editor::new(base.clone(), 1.0, Box::new(BoxText));
        editor.key(Key::Char('x'));
        drag(&mut editor, (380, 100), (420, 140));
        editor.key(Key::Char('b'));
        drag(&mut editor, (380, 300), (420, 340));
        let result = editor.result();
        for y in [110, 320] {
            let middle = result.pixel(400, y).unwrap() & 0xff;
            assert!((1..255).contains(&middle), "row {y} has {middle}");
        }
        assert_eq!(result.pixel(400, 200), base.pixel(400, 200));
        assert_eq!(result.pixel(379, 110), Some(0x00ff_ffff));
    }

    #[test]
    fn step_numbers_count_up_and_recount_after_undo() {
        let mut editor = editor();
        editor.key(Key::Char('n'));
        for x in [100, 200, 300] {
            editor.pointer_down(Point::new(x, 300));
            editor.pointer_up(Point::new(x, 300));
        }
        let widths = |editor: &Editor| -> Vec<u32> {
            editor
                .annotations
                .iter()
                .map(|annotation| match &annotation.shape {
                    Shape::Step(_, mask) => mask.width,
                    _ => 0,
                })
                .collect()
        };
        assert_eq!(widths(&editor).len(), 3);
        assert_eq!(editor.result().pixel(100, 289), Some(PALETTE[0]));
        editor.key(Key::Undo);
        editor.pointer_down(Point::new(300, 400));
        assert_eq!(widths(&editor).len(), 3);
    }

    #[test]
    fn spotlight_dims_outside_and_eraser_uses_the_sampled_color() {
        let mut base = Canvas::new(800, 600, GRAY);
        base.fill_rect(Area::new(0, 0, 800, 200), 0x00ff_ffff, 255);
        let mut editor = Editor::new(base, 1.0, Box::new(BoxText));
        editor.key(Key::Char('d'));
        drag(&mut editor, (50, 150), (150, 280));
        assert_eq!(editor.result().pixel(100, 260), Some(0x00ff_ffff));
        editor.key(Key::Char('o'));
        drag(&mut editor, (300, 300), (500, 500));
        assert_eq!(editor.result().pixel(400, 400), Some(GRAY));
        assert_ne!(editor.result().pixel(700, 400), Some(GRAY));
    }

    #[test]
    fn number_keys_brackets_and_wheel_change_color_and_size() {
        let mut editor = editor();
        editor.key(Key::Char('4'));
        editor.key(Key::Char('9'));
        assert_eq!(editor.color, 3);
        editor.key(Key::Char(']'));
        editor.wheel(100);
        assert_eq!(editor.size, MAX_SIZE);
        editor.wheel(-100);
        editor.key(Key::Char('['));
        assert_eq!(editor.size, 1);
    }
}
