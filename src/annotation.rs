//! Transient Quick Markup model and drawing primitives (FR-11).
//!
//! Shapes live in decoded-image pixel coordinates. The viewer supplies
//! the display scale when previewing them, while clipboard composition
//! draws the same geometry at scale 1. Nothing in this module persists
//! annotations or owns window actions.

use gtk4 as gtk;

use gtk::cairo;
use gtk::graphene;
use gtk::prelude::*;

pub const MAX_SHAPES: usize = 128;
const MAX_UNDO_OPERATIONS: usize = 256;
const MAX_RETAINED_SHAPES: usize = MAX_SHAPES * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Box,
    Arrow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub tool: Tool,
    pub start: Point,
    pub end: Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub tool: Tool,
    pub shape_count: usize,
    pub can_undo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitResult {
    Added,
    TooSmall,
}

#[derive(Clone)]
pub struct Session {
    tool: Tool,
    shapes: Vec<Shape>,
    draft: Option<Shape>,
    undo: Vec<UndoEdit>,
}

#[derive(Clone)]
enum UndoEdit {
    RemoveLast,
    Restore(Vec<Shape>),
}

impl Default for Session {
    fn default() -> Self {
        Self {
            tool: Tool::Box,
            shapes: Vec::new(),
            draft: None,
            undo: Vec::new(),
        }
    }
}

impl Session {
    pub fn status(&self) -> Status {
        Status {
            tool: self.tool,
            shape_count: self.shapes.len(),
            can_undo: self.draft.is_some() || !self.undo.is_empty(),
        }
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.draft = None;
    }

    pub fn begin(&mut self, point: Point) -> bool {
        if self.shapes.len() >= MAX_SHAPES {
            return false;
        }
        self.draft = Some(Shape {
            tool: self.tool,
            start: point,
            end: point,
        });
        true
    }

    pub fn update(&mut self, point: Point) {
        if let Some(draft) = &mut self.draft {
            draft.end = point;
        }
    }

    pub fn commit(&mut self, minimum_distance: f64) -> CommitResult {
        let Some(draft) = self.draft.take() else {
            return CommitResult::TooSmall;
        };
        let dx = draft.end.x - draft.start.x;
        let dy = draft.end.y - draft.start.y;
        if dx.hypot(dy) < minimum_distance.max(0.0) {
            return CommitResult::TooSmall;
        }
        self.shapes.push(draft);
        self.undo.push(UndoEdit::RemoveLast);
        self.prune_undo();
        CommitResult::Added
    }

    pub fn cancel_draft(&mut self) -> bool {
        self.draft.take().is_some()
    }

    pub fn has_draft(&self) -> bool {
        self.draft.is_some()
    }

    pub fn undo(&mut self) -> bool {
        if self.cancel_draft() {
            return true;
        }
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        match edit {
            UndoEdit::RemoveLast => {
                self.shapes.pop();
            }
            UndoEdit::Restore(shapes) => self.shapes = shapes,
        }
        true
    }

    pub fn clear(&mut self) -> bool {
        let changed = !self.shapes.is_empty() || self.draft.is_some();
        if !self.shapes.is_empty() {
            self.undo
                .push(UndoEdit::Restore(std::mem::take(&mut self.shapes)));
            self.prune_undo();
        }
        self.draft = None;
        changed
    }

    fn shapes(&self, include_draft: bool) -> impl Iterator<Item = &Shape> {
        self.shapes
            .iter()
            .chain(self.draft.iter().filter(move |_| include_draft))
    }

    fn prune_undo(&mut self) {
        let retained_shapes = |undo: &[UndoEdit]| {
            undo.iter()
                .map(|edit| match edit {
                    UndoEdit::RemoveLast => 0,
                    UndoEdit::Restore(shapes) => shapes.len(),
                })
                .sum::<usize>()
        };
        while self.undo.len() > MAX_UNDO_OPERATIONS
            || self.shapes.len() + retained_shapes(&self.undo) > MAX_RETAINED_SHAPES
        {
            self.undo.remove(0);
        }
    }
}

/// Append every committed shape and the current draft to `snapshot`.
/// `display_scale` maps one source-image pixel to snapshot coordinates.
pub fn append(
    snapshot: &gtk::Snapshot,
    session: &Session,
    source_size: (f64, f64),
    display_scale: f64,
    include_draft: bool,
) {
    let (width, height) = source_size;
    if width <= 0.0 || height <= 0.0 || display_scale <= 0.0 {
        return;
    }
    let bounds = graphene::Rect::new(
        0.0,
        0.0,
        (width * display_scale) as f32,
        (height * display_scale) as f32,
    );
    let cr = snapshot.append_cairo(&bounds);
    let source_stroke = (width.min(height) * 0.005).clamp(4.0, 16.0);
    let stroke = (source_stroke * display_scale).max(2.0);
    for shape in session.shapes(include_draft) {
        draw_shape(&cr, *shape, display_scale, stroke);
    }
}

fn draw_shape(cr: &cairo::Context, shape: Shape, scale: f64, stroke: f64) {
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);

    // The light under-stroke keeps the red readable over both dark and
    // similarly coloured content without adding style controls to v1.
    add_path(cr, shape, scale, stroke);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
    cr.set_line_width(stroke + 3.0);
    let _ = cr.stroke();

    add_path(cr, shape, scale, stroke);
    cr.set_source_rgb(1.0, 0.16, 0.12);
    cr.set_line_width(stroke);
    let _ = cr.stroke();
}

fn add_path(cr: &cairo::Context, shape: Shape, scale: f64, stroke: f64) {
    let start = Point::new(shape.start.x * scale, shape.start.y * scale);
    let end = Point::new(shape.end.x * scale, shape.end.y * scale);
    match shape.tool {
        Tool::Box => {
            let x = start.x.min(end.x);
            let y = start.y.min(end.y);
            cr.rectangle(x, y, (end.x - start.x).abs(), (end.y - start.y).abs());
        }
        Tool::Arrow => {
            cr.move_to(start.x, start.y);
            cr.line_to(end.x, end.y);

            let dx = end.x - start.x;
            let dy = end.y - start.y;
            let length = dx.hypot(dy);
            if length <= f64::EPSILON {
                return;
            }
            let angle = dy.atan2(dx);
            let head = (stroke * 4.5).min(length * 0.45);
            let wing = std::f64::consts::FRAC_PI_6;
            for direction in [
                angle + std::f64::consts::PI - wing,
                angle + std::f64::consts::PI + wing,
            ] {
                cr.move_to(end.x, end.y);
                cr.line_to(
                    end.x + head * direction.cos(),
                    end.y + head * direction.sin(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn session_commits_undoes_and_clears_shapes() {
        let mut session = Session::default();
        assert_eq!(session.status().tool, Tool::Box);
        assert!(session.begin(point(10.0, 10.0)));
        session.update(point(40.0, 30.0));
        assert_eq!(session.commit(3.0), CommitResult::Added);
        assert_eq!(session.status().shape_count, 1);

        session.set_tool(Tool::Arrow);
        assert!(session.begin(point(1.0, 2.0)));
        session.update(point(20.0, 22.0));
        assert_eq!(session.commit(3.0), CommitResult::Added);
        assert_eq!(session.status().shape_count, 2);
        assert!(session.undo());
        assert_eq!(session.status().shape_count, 1);
        assert!(session.clear());
        assert_eq!(session.status().shape_count, 0);
        assert!(session.status().can_undo);
        session.begin(point(2.0, 2.0));
        session.update(point(2.5, 2.5));
        assert_eq!(session.commit(3.0), CommitResult::TooSmall);
        assert!(session.undo(), "clear should be reversible");
        assert_eq!(session.status().shape_count, 1);
        assert!(session.clear());
        assert!(!session.clear());
    }

    #[test]
    fn tiny_and_cancelled_drafts_never_enter_history() {
        let mut session = Session::default();
        session.begin(point(5.0, 5.0));
        session.update(point(6.0, 6.0));
        assert_eq!(session.commit(3.0), CommitResult::TooSmall);
        assert_eq!(session.status().shape_count, 0);

        session.begin(point(5.0, 5.0));
        assert!(session.has_draft());
        assert!(session.cancel_draft());
        assert!(!session.has_draft());
        assert!(!session.cancel_draft());
    }

    #[test]
    fn history_is_bounded() {
        let mut session = Session::default();
        for index in 0..MAX_SHAPES {
            assert!(session.begin(point(index as f64, 0.0)));
            session.update(point(index as f64 + 10.0, 0.0));
            assert_eq!(session.commit(1.0), CommitResult::Added);
        }
        assert_eq!(session.status().shape_count, MAX_SHAPES);
        assert!(!session.begin(point(0.0, 0.0)));
    }

    #[test]
    fn both_tools_draw_visible_pixels() {
        for tool in [Tool::Box, Tool::Arrow] {
            let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            draw_shape(
                &cr,
                Shape {
                    tool,
                    start: point(8.0, 8.0),
                    end: point(54.0, 48.0),
                },
                1.0,
                4.0,
            );
            drop(cr);
            surface.flush();
            assert!(
                surface.data().unwrap().iter().any(|byte| *byte != 0),
                "{tool:?} should produce non-transparent pixels"
            );
        }
    }
}
