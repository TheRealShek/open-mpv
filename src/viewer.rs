//! Display widget (FR-4): fit/actual/manual zoom, cursor-anchored
//! zoom, pan with edge clamping, 90° view rotation, and physical-pixel
//! alignment so 100% is pixel-exact under fractional scaling (FR-4.7).
//!
//! Renders any `GdkPaintable`: still images arrive as `GdkTexture`
//! (which is a paintable) and keep an explicit scaling-filter path;
//! live paintables such as the video sink's (FR-10) redraw through
//! their invalidate signals.
//!
//! Zoom is expressed against physical pixels: zoom 1.0 means one source
//! pixel maps to one physical display pixel regardless of scale factor.

use gtk4 as gtk;

use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::annotation::{self, Point, Session, Status, Tool};

pub const ZOOM_MIN: f64 = 0.05;
pub const ZOOM_MAX: f64 = 20.0;

/// Zoom applied per wheel detent (FR-4.2).
const ZOOM_STEP: f64 = 1.1;
/// GDK reports mouse-wheel scrolling in detent clicks but touchpad
/// scrolling in *logical pixels* (`GdkScrollUnit`), and the two arrive
/// through the same signal. This many pixels stands in for one detent.
const SURFACE_PIXELS_PER_DETENT: f64 = 50.0;
/// Travel before a touchpad gesture commits to zooming or navigating, so
/// sideways jitter during a two-finger zoom cannot flip the image.
const AXIS_LOCK_DETENTS: f64 = 0.2;
/// Sideways travel that counts as one swipe to the next image.
const SWIPE_DETENTS: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, PartialEq)]
enum ScrollAction {
    Ignore,
    /// +1 next, -1 previous.
    Navigate(i32),
    /// Multiply the current zoom by this factor.
    Zoom(f64),
}

/// Scroll travel accumulated within one touchpad gesture. A wheel detent
/// is a complete gesture in itself and does not touch this.
#[derive(Debug, Default)]
struct ScrollGesture {
    /// Unspent travel, in detent equivalents.
    dx: f64,
    dy: f64,
    axis: Option<Axis>,
}

impl ScrollGesture {
    /// What one scroll event should do. Wheel detents act immediately,
    /// one image or one zoom step each. Touchpad pixels are accumulated:
    /// a swipe delivers dozens of events, and acting on each one would
    /// race through the folder or zoom by orders of magnitude.
    fn event(&mut self, unit: gtk::gdk::ScrollUnit, dx: f64, dy: f64) -> ScrollAction {
        if unit == gtk::gdk::ScrollUnit::Wheel {
            return if dx.abs() > dy.abs() {
                ScrollAction::Navigate(if dx > 0.0 { 1 } else { -1 })
            } else if dy != 0.0 {
                ScrollAction::Zoom(ZOOM_STEP.powf(-dy))
            } else {
                ScrollAction::Ignore
            };
        }

        self.dx += dx / SURFACE_PIXELS_PER_DETENT;
        self.dy += dy / SURFACE_PIXELS_PER_DETENT;
        if self.axis.is_none() && self.dx.abs().max(self.dy.abs()) >= AXIS_LOCK_DETENTS {
            self.axis = Some(if self.dx.abs() > self.dy.abs() {
                Axis::Horizontal
            } else {
                Axis::Vertical
            });
        }
        match self.axis {
            // Too early to tell what the fingers are doing; the travel
            // stays banked and is spent once the axis settles.
            None => ScrollAction::Ignore,
            Some(Axis::Horizontal) => {
                if self.dx.abs() < SWIPE_DETENTS {
                    return ScrollAction::Ignore;
                }
                let dir = if self.dx > 0.0 { 1 } else { -1 };
                self.dx -= f64::from(dir) * SWIPE_DETENTS;
                ScrollAction::Navigate(dir)
            }
            Some(Axis::Vertical) => {
                let travel = std::mem::take(&mut self.dy);
                if travel == 0.0 {
                    ScrollAction::Ignore
                } else {
                    ScrollAction::Zoom(ZOOM_STEP.powf(-travel))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Scale to the window according to the current medium's fit policy.
    Fit,
    /// Explicit zoom level; 1.0 is pixel-exact 100%.
    Manual(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FitPolicy {
    /// Images retain pixel-exact presentation when they already fit (FR-4.1).
    DownscaleOnly,
    /// Video uses the largest aspect-preserving size the viewport allows (FR-10.4).
    ScaleToViewport,
}

pub struct State {
    paintable: Option<gtk::gdk::Paintable>,
    /// Logical pixel size of the image independent of texture resolution.
    /// For raster images this equals the texture size; for SVG it is the
    /// document's nominal size, so re-rendered hi-res textures keep the
    /// same on-screen geometry (FR-2.3).
    nominal: Option<(f64, f64)>,
    mode: Mode,
    fit_policy: FitPolicy,
    /// Pan offset of the image center from the widget center, logical px.
    offset: (f64, f64),
    /// View rotation in quarter turns, 0..4.
    rotation: u8,
    default_fit_actual: bool,
    pointer: (f64, f64),
    drag_origin: (f64, f64),
    annotation: Option<Session>,
}

impl Default for State {
    fn default() -> Self {
        State {
            paintable: None,
            nominal: None,
            mode: Mode::Fit,
            fit_policy: FitPolicy::DownscaleOnly,
            offset: (0.0, 0.0),
            rotation: 0,
            default_fit_actual: false,
            pointer: (0.0, 0.0),
            drag_origin: (0.0, 0.0),
            annotation: None,
        }
    }
}

mod imp {
    use super::*;
    use std::cell::RefCell;

    pub(super) type Callback<T> = RefCell<Option<Box<dyn Fn(T)>>>;

    #[derive(Default)]
    pub struct ImageView {
        pub(super) state: RefCell<State>,
        /// Signal connections into a live paintable (video sink); kept
        /// with their source so replacing the paintable disconnects
        /// them and a stale source cannot keep redrawing us.
        pub(super) live: RefCell<Option<(gtk::gdk::Paintable, Vec<glib::SignalHandlerId>)>>,
        /// Fired when zoom/rotation changes; argument is zoom percent.
        pub(super) on_view_changed: Callback<f64>,
        /// Fired on horizontal scroll: +1 next, -1 prev (FR-3.1).
        pub(super) on_navigate: Callback<i32>,
        /// Fired when a live source reports its dimensions; argument is
        /// the source size in logical pixels.
        pub(super) on_source_size: Callback<(f64, f64)>,
        /// Fired after a Quick Markup command changes its tool or history.
        pub(super) on_annotation_changed: Callback<Status>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImageView {
        const NAME: &'static str = "OpenMpvImageView";
        type Type = super::ImageView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ImageView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.add_controllers();
        }
    }

    impl WidgetImpl for ImageView {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            // Copy everything out of the state first: a foreign
            // paintable's snapshot must not run under our borrow.
            let (paintable, transform, zoom) = {
                let state = self.state.borrow();
                let Some(paintable) = state.paintable.clone() else {
                    return;
                };
                let scale = obj.surface_scale();
                let Some(transform) = super::view_transform_for(
                    &state,
                    &paintable,
                    (obj.width() as f64, obj.height() as f64),
                    scale,
                ) else {
                    // Zero source size: a video paintable before preroll.
                    return;
                };
                let zoom = transform.displayed.0 * scale / transform.source.0;
                (paintable, transform, zoom)
            };
            let (tw, th) = transform.source;
            let (dw, dh) = transform.displayed;

            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                transform.center.0 as f32,
                transform.center.1 as f32,
            ));
            if transform.rotation != 0 {
                snapshot.rotate(transform.rotation as f32 * 90.0);
            }
            snapshot.translate(&graphene::Point::new(
                (-dw / 2.0) as f32,
                (-dh / 2.0) as f32,
            ));
            if let Some(texture) = paintable.downcast_ref::<gtk::gdk::Texture>() {
                // Still images pick the scaling filter explicitly so
                // 100% stays pixel-exact and downscales stay smooth.
                let filter = if zoom < 1.0 {
                    gsk::ScalingFilter::Trilinear
                } else if zoom > 1.0 {
                    gsk::ScalingFilter::Linear
                } else {
                    gsk::ScalingFilter::Nearest
                };
                snapshot.append_scaled_texture(
                    texture,
                    filter,
                    &graphene::Rect::new(0.0, 0.0, dw as f32, dh as f32),
                );
            } else {
                // Compose the foreign paintable at its native size, then
                // wrap that node in an explicit GSK scale. This leaves no
                // sizing policy to gtk4paintablesink and keeps the dmabuf
                // scaling on the GPU (FR-10.1/10.4).
                let frame = gtk::Snapshot::new();
                paintable.snapshot(&frame, tw, th);
                if let Some(frame) = frame.to_node() {
                    let scaled = gtk::Snapshot::new();
                    scaled.scale((dw / tw) as f32, (dh / th) as f32);
                    scaled.append_node(&frame);
                    if let Some(scaled) = scaled.to_node() {
                        snapshot.append_node(&scaled);
                    }
                }
            }
            // The foreign paintable has finished snapshotting before this
            // borrow. GTK callbacks may re-enter the widget, so never hold
            // its RefCell across paintable.snapshot above.
            if let Some(session) = self.state.borrow().annotation.as_ref() {
                annotation::append(snapshot, session, (tw, th), dw / tw, true);
            }
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct ImageView(ObjectSubclass<imp::ImageView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ImageView {
    fn default() -> Self {
        glib::Object::new()
    }
}

fn image_dims(state: &State, paintable: &gtk::gdk::Paintable) -> (f64, f64) {
    state.nominal.unwrap_or((
        paintable.intrinsic_width() as f64,
        paintable.intrinsic_height() as f64,
    ))
}

fn effective_zoom(state: &State, w: f64, h: f64, scale: f64, tw: f64, th: f64) -> f64 {
    let (rtw, rth) = if state.rotation % 2 == 1 {
        (th, tw)
    } else {
        (tw, th)
    };
    match (state.mode, state.fit_policy) {
        (Mode::Fit, FitPolicy::DownscaleOnly) => (w * scale / rtw).min(h * scale / rth).min(1.0),
        (Mode::Fit, FitPolicy::ScaleToViewport) => (w * scale / rtw).min(h * scale / rth),
        (Mode::Manual(z), _) => z,
    }
}

/// Keep the image within view: centered while it fits, no gaps past the
/// edges once it is larger than the viewport.
fn clamp_offset(offset: (f64, f64), rw: f64, rh: f64, w: f64, h: f64) -> (f64, f64) {
    let clamp1 = |off: f64, size: f64, view: f64| {
        if size <= view {
            0.0
        } else {
            off.clamp(-(size - view) / 2.0, (size - view) / 2.0)
        }
    };
    (clamp1(offset.0, rw, w), clamp1(offset.1, rh, h))
}

#[derive(Debug, Clone, Copy)]
struct ViewTransform {
    source: (f64, f64),
    displayed: (f64, f64),
    center: (f64, f64),
    rotation: u8,
}

impl ViewTransform {
    /// Convert a widget point into decoded-image pixels. A drag must begin
    /// inside the image; once begun, its endpoint clamps to the image so a
    /// quick overshoot does not discard the annotation.
    fn source_point(self, widget: (f64, f64), clamp: bool) -> Option<Point> {
        let (mut x, mut y) = (widget.0 - self.center.0, widget.1 - self.center.1);
        (x, y) = match self.rotation % 4 {
            0 => (x, y),
            1 => (y, -x),
            2 => (-x, -y),
            _ => (-y, x),
        };
        let source = Point::new(
            (x + self.displayed.0 / 2.0) * self.source.0 / self.displayed.0,
            (y + self.displayed.1 / 2.0) * self.source.1 / self.displayed.1,
        );
        let inside = source.x >= 0.0
            && source.x <= self.source.0
            && source.y >= 0.0
            && source.y <= self.source.1;
        if !inside && !clamp {
            return None;
        }
        Some(Point::new(
            source.x.clamp(0.0, self.source.0),
            source.y.clamp(0.0, self.source.1),
        ))
    }

    fn source_distance(self, logical_pixels: f64) -> f64 {
        logical_pixels * self.source.0 / self.displayed.0
    }
}

fn view_transform_for(
    state: &State,
    paintable: &gtk::gdk::Paintable,
    viewport: (f64, f64),
    surface_scale: f64,
) -> Option<ViewTransform> {
    let source = image_dims(state, paintable);
    if viewport.0 <= 0.0
        || viewport.1 <= 0.0
        || source.0 <= 0.0
        || source.1 <= 0.0
        || surface_scale <= 0.0
    {
        return None;
    }
    let zoom = effective_zoom(
        state,
        viewport.0,
        viewport.1,
        surface_scale,
        source.0,
        source.1,
    );
    let displayed = (
        source.0 * zoom / surface_scale,
        source.1 * zoom / surface_scale,
    );
    let rotated = if state.rotation % 2 == 1 {
        (displayed.1, displayed.0)
    } else {
        displayed
    };
    let offset = clamp_offset(state.offset, rotated.0, rotated.1, viewport.0, viewport.1);
    let center = (
        ((viewport.0 / 2.0 + offset.0) * surface_scale).round() / surface_scale,
        ((viewport.1 / 2.0 + offset.1) * surface_scale).round() / surface_scale,
    );
    Some(ViewTransform {
        source,
        displayed,
        center,
        rotation: state.rotation,
    })
}

#[derive(Debug)]
pub enum MarkupCopyError {
    NoImage,
    NoAnnotations,
    RendererUnavailable,
    EmptyRender,
}

impl std::fmt::Display for MarkupCopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoImage => write!(f, "No image is available for Quick Markup"),
            Self::NoAnnotations => write!(f, "Draw a box or arrow before copying"),
            Self::RendererUnavailable => write!(f, "Cannot prepare the annotated image"),
            Self::EmptyRender => write!(f, "The annotated image could not be rendered"),
        }
    }
}

impl ImageView {
    fn surface_scale(&self) -> f64 {
        self.native()
            .and_then(|n| n.surface())
            .map(|s| s.scale())
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0)
    }

    fn state(&self) -> std::cell::RefMut<'_, State> {
        self.imp().state.borrow_mut()
    }

    /// Show a new image, resetting the view to the default fit (FR-4.5).
    /// `nominal` overrides the on-screen size for resolution-independent
    /// content (SVG); raster images pass None.
    pub fn show_texture(&self, texture: gtk::gdk::Texture, nominal: Option<(f64, f64)>) {
        self.show_paintable(texture.upcast(), nominal, FitPolicy::DownscaleOnly);
    }

    /// Show a live video paintable, scaling it to the viewport in Fit mode
    /// and following invalidations until it is replaced (FR-10.4).
    pub fn show_live_paintable(&self, paintable: gtk::gdk::Paintable) {
        self.show_paintable(paintable, None, FitPolicy::ScaleToViewport);
    }

    fn show_paintable(
        &self,
        paintable: gtk::gdk::Paintable,
        nominal: Option<(f64, f64)>,
        fit_policy: FitPolicy,
    ) {
        self.watch_live_source(&paintable);
        {
            let mut st = self.state();
            st.paintable = Some(paintable);
            st.nominal = nominal;
            st.fit_policy = fit_policy;
            st.rotation = 0;
            st.offset = (0.0, 0.0);
            st.annotation = None;
            st.mode = if st.default_fit_actual {
                Mode::Manual(1.0)
            } else {
                Mode::Fit
            };
        }
        self.queue_draw();
        self.emit_view_changed();
    }

    /// Subscribe to a live paintable's redraw signals, dropping any
    /// previous subscription. Textures are static: no subscription.
    fn watch_live_source(&self, paintable: &gtk::gdk::Paintable) {
        if let Some((old, ids)) = self.imp().live.borrow_mut().take() {
            for id in ids {
                old.disconnect(id);
            }
        }
        if paintable.is::<gtk::gdk::Texture>() {
            return;
        }
        let weak = self.downgrade();
        let contents = paintable.connect_invalidate_contents(move |_| {
            if let Some(view) = weak.upgrade() {
                view.queue_draw();
            }
        });
        let weak = self.downgrade();
        let size = paintable.connect_invalidate_size(move |paintable| {
            if let Some(view) = weak.upgrade() {
                // New source dimensions change the effective zoom.
                view.queue_draw();
                view.emit_view_changed();
                // A video only learns its dimensions at preroll, after
                // the window is already on screen (FR-6.6).
                let (w, h) = (
                    paintable.intrinsic_width() as f64,
                    paintable.intrinsic_height() as f64,
                );
                if w > 0.0
                    && h > 0.0
                    && let Some(f) = view.imp().on_source_size.borrow().as_ref()
                {
                    f((w, h));
                }
            }
        });
        *self.imp().live.borrow_mut() = Some((paintable.clone(), vec![contents, size]));
    }

    /// Remove the image (empty and error states).
    pub fn clear(&self) {
        if let Some((old, ids)) = self.imp().live.borrow_mut().take() {
            for id in ids {
                old.disconnect(id);
            }
        }
        self.state().paintable = None;
        self.state().annotation = None;
        self.queue_draw();
    }

    /// Swap the texture without touching view state (animation frames,
    /// SVG re-render).
    pub fn update_texture(&self, texture: gtk::gdk::Texture) {
        self.state().paintable = Some(texture.upcast());
        self.queue_draw();
    }

    /// Initial fit mode from config (FR-8.2 `fit=`).
    pub fn set_default_fit_actual(&self, actual: bool) {
        self.state().default_fit_actual = actual;
    }

    pub fn zoom_percent(&self) -> f64 {
        let st = self.imp().state.borrow();
        let Some(p) = st.paintable.as_ref() else {
            return 100.0;
        };
        let (tw, th) = image_dims(&st, p);
        if tw <= 0.0 || th <= 0.0 {
            // Video paintable before preroll has no size yet.
            return 100.0;
        }
        effective_zoom(
            &st,
            self.width() as f64,
            self.height() as f64,
            self.surface_scale(),
            tw,
            th,
        ) * 100.0
    }

    /// Current view rotation in quarter turns (0..4).
    pub fn rotation(&self) -> u8 {
        self.imp().state.borrow().rotation
    }

    fn view_transform(&self) -> Option<ViewTransform> {
        let st = self.imp().state.borrow();
        let paintable = st.paintable.as_ref()?;
        view_transform_for(
            &st,
            paintable,
            (self.width() as f64, self.height() as f64),
            self.surface_scale(),
        )
    }

    pub fn start_markup(&self) -> bool {
        let mut st = self.state();
        if !st
            .paintable
            .as_ref()
            .is_some_and(|p| p.is::<gtk::gdk::Texture>())
        {
            return false;
        }
        st.annotation = Some(Session::default());
        let status = st.annotation.as_ref().map(Session::status);
        drop(st);
        self.queue_draw();
        if let Some(status) = status {
            self.emit_annotation_changed(status);
        }
        true
    }

    pub fn cancel_markup(&self) -> bool {
        let changed = self.state().annotation.take().is_some();
        if changed {
            self.queue_draw();
        }
        changed
    }

    pub fn is_marking_up(&self) -> bool {
        self.imp().state.borrow().annotation.is_some()
    }

    pub fn contains_image_point(&self, x: f64, y: f64) -> bool {
        self.view_transform()
            .and_then(|transform| transform.source_point((x, y), false))
            .is_some()
    }

    pub fn markup_has_draft(&self) -> bool {
        self.imp()
            .state
            .borrow()
            .annotation
            .as_ref()
            .is_some_and(Session::has_draft)
    }

    pub fn cancel_markup_draft(&self) -> bool {
        let mut st = self.state();
        let Some(session) = st.annotation.as_mut() else {
            return false;
        };
        let changed = session.cancel_draft();
        let status = session.status();
        drop(st);
        if changed {
            self.queue_draw();
            self.emit_annotation_changed(status);
        }
        changed
    }

    pub fn set_markup_tool(&self, tool: Tool) {
        let status = {
            let mut st = self.state();
            let Some(session) = st.annotation.as_mut() else {
                return;
            };
            session.set_tool(tool);
            session.status()
        };
        self.queue_draw();
        self.emit_annotation_changed(status);
    }

    pub fn undo_markup(&self) -> bool {
        let status = {
            let mut st = self.state();
            let Some(session) = st.annotation.as_mut() else {
                return false;
            };
            if !session.undo() {
                return false;
            }
            session.status()
        };
        self.queue_draw();
        self.emit_annotation_changed(status);
        true
    }

    pub fn clear_markup(&self) -> bool {
        let status = {
            let mut st = self.state();
            let Some(session) = st.annotation.as_mut() else {
                return false;
            };
            if !session.clear() {
                return false;
            }
            session.status()
        };
        self.queue_draw();
        self.emit_annotation_changed(status);
        true
    }

    pub fn markup_status(&self) -> Option<Status> {
        self.imp()
            .state
            .borrow()
            .annotation
            .as_ref()
            .map(Session::status)
    }

    /// Render only the decoded image and annotations at source resolution.
    /// Window chrome, background, zoom and pan are deliberately absent.
    pub fn annotated_texture(&self) -> Result<gtk::gdk::Texture, MarkupCopyError> {
        let (texture, session, rotation) = {
            let st = self.imp().state.borrow();
            let texture = st
                .paintable
                .as_ref()
                .and_then(|paintable| paintable.downcast_ref::<gtk::gdk::Texture>())
                .cloned()
                .ok_or(MarkupCopyError::NoImage)?;
            let session = st.annotation.clone().ok_or(MarkupCopyError::NoImage)?;
            if session.status().shape_count == 0 {
                return Err(MarkupCopyError::NoAnnotations);
            }
            (texture, session, st.rotation)
        };
        let (width, height) = (texture.width() as f64, texture.height() as f64);
        let (output_width, output_height) = if rotation % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        };

        let snapshot = gtk::Snapshot::new();
        snapshot.translate(&graphene::Point::new(
            (output_width / 2.0) as f32,
            (output_height / 2.0) as f32,
        ));
        if rotation != 0 {
            snapshot.rotate(rotation as f32 * 90.0);
        }
        snapshot.translate(&graphene::Point::new(
            (-width / 2.0) as f32,
            (-height / 2.0) as f32,
        ));
        snapshot.append_texture(
            &texture,
            &graphene::Rect::new(0.0, 0.0, width as f32, height as f32),
        );
        annotation::append(&snapshot, &session, (width, height), 1.0, false);
        let node = snapshot.to_node().ok_or(MarkupCopyError::EmptyRender)?;
        let renderer = self
            .native()
            .and_then(|native| native.renderer())
            .ok_or(MarkupCopyError::RendererUnavailable)?;
        Ok(renderer.render_texture(
            &node,
            Some(&graphene::Rect::new(
                0.0,
                0.0,
                output_width as f32,
                output_height as f32,
            )),
        ))
    }

    /// On-screen size of the image in logical pixels, after zoom and
    /// rotation. `None` when there is nothing to measure.
    fn displayed_size(&self) -> Option<(f64, f64)> {
        let st = self.imp().state.borrow();
        let p = st.paintable.as_ref()?;
        let (tw, th) = image_dims(&st, p);
        if tw <= 0.0 || th <= 0.0 {
            return None;
        }
        let (w, h) = (self.width() as f64, self.height() as f64);
        let scale = self.surface_scale();
        let z = effective_zoom(&st, w, h, scale, tw, th);
        let (dw, dh) = (tw * z / scale, th * z / scale);
        Some(if st.rotation % 2 == 1 {
            (dh, dw)
        } else {
            (dw, dh)
        })
    }

    pub fn is_pannable(&self) -> bool {
        let Some((rw, rh)) = self.displayed_size() else {
            return false;
        };
        rw > self.width() as f64 + 0.5 || rh > self.height() as f64 + 0.5
    }

    /// Shift the view by a step in logical pixels (FR-4.3). Unlike the
    /// drag path this clamps as it writes: an arrow key held against an
    /// edge would otherwise bank offset the draw silently discards, and
    /// the first press back would spend it instead of moving.
    pub fn pan_by(&self, dx: f64, dy: f64) {
        let Some((rw, rh)) = self.displayed_size() else {
            return;
        };
        let (w, h) = (self.width() as f64, self.height() as f64);
        {
            let mut st = self.state();
            st.offset = clamp_offset((st.offset.0 + dx, st.offset.1 + dy), rw, rh, w, h);
        }
        self.queue_draw();
    }

    pub fn zoom_fit(&self) {
        {
            let mut st = self.state();
            st.mode = Mode::Fit;
            st.offset = (0.0, 0.0);
        }
        self.queue_draw();
        self.emit_view_changed();
    }

    pub fn toggle_fit_actual(&self) {
        let is_fit = matches!(self.imp().state.borrow().mode, Mode::Fit);
        if is_fit {
            self.zoom_to(1.0, None);
        } else {
            self.zoom_fit();
        }
    }

    pub fn zoom_by(&self, factor: f64, anchor: Option<(f64, f64)>) {
        let current = self.zoom_percent() / 100.0;
        self.zoom_to(current * factor, anchor);
    }

    /// Set an absolute zoom, keeping `anchor` (widget coords) fixed on
    /// the same image point; anchor defaults to the widget center.
    pub fn zoom_to(&self, zoom: f64, anchor: Option<(f64, f64)>) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let old = self.zoom_percent() / 100.0;
        let new = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        {
            let mut st = self.state();
            let (ax, ay) = anchor.unwrap_or((w / 2.0, h / 2.0));
            // Keep the image point under the anchor stationary.
            let ratio = if old > 0.0 { new / old } else { 1.0 };
            let (cx, cy) = (w / 2.0 + st.offset.0, h / 2.0 + st.offset.1);
            st.offset = (
                ax - w / 2.0 - (ax - cx) * ratio,
                ay - h / 2.0 - (ay - cy) * ratio,
            );
            st.mode = Mode::Manual(new);
        }
        self.queue_draw();
        self.emit_view_changed();
    }

    /// Rotate the view by ±1 quarter turn (FR-5.3).
    pub fn rotate_view(&self, quarter_turns: i8) {
        {
            let mut st = self.state();
            st.rotation = (st.rotation as i8 + quarter_turns).rem_euclid(4) as u8;
            st.offset = (0.0, 0.0);
        }
        self.queue_draw();
        self.emit_view_changed();
    }

    pub fn connect_view_changed(&self, f: impl Fn(f64) + 'static) {
        *self.imp().on_view_changed.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_source_size(&self, f: impl Fn((f64, f64)) + 'static) {
        *self.imp().on_source_size.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_navigate(&self, f: impl Fn(i32) + 'static) {
        *self.imp().on_navigate.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_annotation_changed(&self, f: impl Fn(Status) + 'static) {
        *self.imp().on_annotation_changed.borrow_mut() = Some(Box::new(f));
    }

    fn emit_view_changed(&self) {
        let percent = self.zoom_percent();
        if let Some(f) = self.imp().on_view_changed.borrow().as_ref() {
            f(percent);
        }
    }

    fn emit_annotation_changed(&self, status: Status) {
        if let Some(f) = self.imp().on_annotation_changed.borrow().as_ref() {
            f(status);
        }
    }

    fn add_controllers(&self) {
        // Track the pointer for cursor-anchored scroll zoom (FR-4.2).
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, x, y| {
                view.state().pointer = (x, y);
            }
        ));
        self.add_controller(motion);

        // Vertical scroll zooms at the cursor; horizontal (or Shift+
        // vertical, which GTK delivers as horizontal) navigates (FR-3.1).
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let gesture = std::rc::Rc::new(std::cell::RefCell::new(ScrollGesture::default()));
        // Each touchpad gesture starts with a clean slate; without this
        // the previous swipe's axis would still be locked in.
        scroll.connect_scroll_begin(glib::clone!(
            #[strong]
            gesture,
            move |_| *gesture.borrow_mut() = ScrollGesture::default()
        ));
        scroll.connect_scroll_end(glib::clone!(
            #[strong]
            gesture,
            move |_| *gesture.borrow_mut() = ScrollGesture::default()
        ));
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            gesture,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, dx, dy| {
                // Resolve the action before acting on it: the callbacks
                // below re-enter the widget, and a live borrow here would
                // abort the process inside this GTK callback.
                let action = gesture.borrow_mut().event(controller.unit(), dx, dy);
                match action {
                    ScrollAction::Navigate(dir) => {
                        if !view.is_marking_up()
                            && let Some(f) = view.imp().on_navigate.borrow().as_ref()
                        {
                            f(dir);
                        }
                    }
                    ScrollAction::Zoom(factor) => {
                        let anchor = view.imp().state.borrow().pointer;
                        view.zoom_by(factor, Some(anchor));
                    }
                    ScrollAction::Ignore => {}
                }
                glib::Propagation::Stop
            }
        ));
        self.add_controller(scroll);

        // Quick Markup owns primary drag while active. Capture phase puts
        // it ahead of the ordinary pan and window-move gestures, while the
        // window's resize-edge capture still wins at the outer margin.
        let markup_drag = gtk::GestureDrag::new();
        markup_drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        markup_drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, x, y| {
                if !view.is_marking_up() {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                }
                let Some(point) = view
                    .view_transform()
                    .and_then(|transform| transform.source_point((x, y), false))
                else {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                };
                let began = view
                    .state()
                    .annotation
                    .as_mut()
                    .is_some_and(|session| session.begin(point));
                if began {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    view.queue_draw();
                    if let Some(status) = view.markup_status() {
                        view.emit_annotation_changed(status);
                    }
                }
            }
        ));
        markup_drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, dx, dy| {
                if !view.markup_has_draft() {
                    return;
                }
                let (start_x, start_y) = gesture.start_point().unwrap_or((0.0, 0.0));
                let Some(point) = view.view_transform().and_then(|transform| {
                    transform.source_point((start_x + dx, start_y + dy), true)
                }) else {
                    return;
                };
                if let Some(session) = view.state().annotation.as_mut() {
                    session.update(point);
                }
                view.queue_draw();
            }
        ));
        markup_drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, _| {
                let minimum = view
                    .view_transform()
                    .map_or(3.0, |transform| transform.source_distance(3.0));
                {
                    let mut st = view.state();
                    if let Some(session) = st.annotation.as_mut() {
                        session.commit(minimum);
                    }
                }
                view.queue_draw();
                if let Some(status) = view.markup_status() {
                    view.emit_annotation_changed(status);
                }
            }
        ));
        markup_drag.connect_cancel(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                view.cancel_markup_draft();
            }
        ));
        self.add_controller(markup_drag);

        // Drag pans when the image overflows the viewport; otherwise the
        // gesture is denied so the window's drag-to-move can take over
        // (FR-6.4).
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, _, _| {
                if view.is_marking_up() {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                } else if view.is_pannable() {
                    // Single borrow: the RHS temporary of a two-borrow
                    // assignment lives until end of statement and aborts
                    // the process inside this non-unwinding GTK callback.
                    let mut st = view.state();
                    st.drag_origin = st.offset;
                } else {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                }
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, dx, dy| {
                let origin = view.imp().state.borrow().drag_origin;
                view.state().offset = (origin.0 + dx, origin.1 + dy);
                view.queue_draw();
            }
        ));
        self.add_controller(drag);

        // Touchpad pinch zoom, anchored at the gesture center (FR-4.2).
        let pinch = gtk::GestureZoom::new();
        let base = std::rc::Rc::new(std::cell::Cell::new(1.0f64));
        pinch.connect_begin(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            base,
            move |_, _| {
                base.set(view.zoom_percent() / 100.0);
            }
        ));
        pinch.connect_scale_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            base,
            move |gesture, scale| {
                let anchor = gesture.bounding_box_center();
                view.zoom_to(base.get() * scale, anchor);
            }
        ));
        self.add_controller(pinch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gtk::gdk::ScrollUnit::{Surface, Wheel};

    /// One touchpad event, in the logical pixels GDK reports.
    fn swipe(g: &mut ScrollGesture, dx: f64, dy: f64) -> ScrollAction {
        g.event(Surface, dx, dy)
    }

    #[test]
    fn wheel_detents_act_one_at_a_time() {
        let mut g = ScrollGesture::default();
        assert_eq!(g.event(Wheel, 0.0, -1.0), ScrollAction::Zoom(ZOOM_STEP));
        assert_eq!(g.event(Wheel, 1.0, 0.0), ScrollAction::Navigate(1));
        assert_eq!(g.event(Wheel, -1.0, 0.0), ScrollAction::Navigate(-1));
        assert_eq!(g.event(Wheel, 0.0, 0.0), ScrollAction::Ignore);
    }

    /// Images advanced by a sideways swipe of `pixels`, delivered the way
    /// a touchpad delivers it: many small events.
    fn images_moved(pixels: f64) -> usize {
        let mut g = ScrollGesture::default();
        (0..(pixels / 10.0).round() as usize)
            .filter(|_| swipe(&mut g, 10.0, 0.0) == ScrollAction::Navigate(1))
            .count()
    }

    #[test]
    fn a_touchpad_swipe_advances_by_travel_not_by_event_count() {
        // One image per SWIPE_DETENTS of travel (100 logical px), however
        // many events that took. The defect this guards: a 150 px swipe
        // arrives as ~15 events and used to move 15 images.
        assert_eq!(images_moved(150.0), 1);
        assert_eq!(images_moved(250.0), 2);
        assert_eq!(images_moved(350.0), 3);
        // A twitch is not a swipe.
        assert_eq!(images_moved(30.0), 0);
    }

    #[test]
    fn touchpad_zoom_is_gentle_where_wheel_zoom_is_a_full_step() {
        let mut g = ScrollGesture::default();
        // Reading a 10 px flick as ten detents would zoom out 2.6x.
        let ScrollAction::Zoom(factor) = swipe(&mut g, 0.0, 10.0) else {
            panic!("expected a zoom");
        };
        assert!(
            (factor - ZOOM_STEP.powf(-10.0 / SURFACE_PIXELS_PER_DETENT)).abs() < 1e-12,
            "{factor} should be one fifth of a detent"
        );
        assert!(
            factor > 0.97 && factor < 1.0,
            "{factor} is not a gentle step"
        );
    }

    #[test]
    fn sideways_jitter_during_a_zoom_never_navigates() {
        let mut g = ScrollGesture::default();
        for _ in 0..20 {
            // Fingers drifting a pixel sideways per event while zooming.
            assert_ne!(swipe(&mut g, 1.0, -12.0), ScrollAction::Navigate(-1));
            assert_ne!(swipe(&mut g, -1.0, -12.0), ScrollAction::Navigate(1));
        }
        assert_eq!(g.axis, Some(Axis::Vertical));
    }

    #[test]
    fn a_new_gesture_is_free_to_pick_the_other_axis() {
        let mut g = ScrollGesture::default();
        for _ in 0..5 {
            swipe(&mut g, 20.0, 0.0);
        }
        assert_eq!(g.axis, Some(Axis::Horizontal));
        // What connect_scroll_begin/end do between gestures.
        g = ScrollGesture::default();
        swipe(&mut g, 0.0, 30.0);
        assert_eq!(g.axis, Some(Axis::Vertical));
    }

    #[test]
    fn image_fit_never_upscales() {
        let st = State::default();
        // 100x100 texture in a 400x400 logical window at scale 1.
        assert_eq!(effective_zoom(&st, 400.0, 400.0, 1.0, 100.0, 100.0), 1.0);
        // Large texture scales down.
        let z = effective_zoom(&st, 400.0, 400.0, 1.0, 800.0, 400.0);
        assert!((z - 0.5).abs() < 1e-9);
    }

    #[test]
    fn video_fit_upscales_and_preserves_aspect_ratio() {
        let st = State {
            fit_policy: FitPolicy::ScaleToViewport,
            ..Default::default()
        };
        // A 720p video fills a 1080p viewport without cropping.
        let z = effective_zoom(&st, 1920.0, 1080.0, 1.0, 1280.0, 720.0);
        assert!((z - 1.5).abs() < 1e-9);
        // A taller viewport remains width-bound, leaving only the
        // aspect-ratio bars that are mathematically necessary.
        let z = effective_zoom(&st, 1920.0, 1200.0, 1.0, 1280.0, 720.0);
        assert!((z - 1.5).abs() < 1e-9);
        assert!((720.0 * z - 1080.0).abs() < 1e-9);
    }

    #[test]
    fn fit_accounts_for_rotation_and_scale_factor() {
        let mut st = State {
            rotation: 1,
            ..Default::default()
        };
        // 800x200 texture rotated 90° behaves as 200x800.
        let z = effective_zoom(&st, 400.0, 400.0, 1.0, 800.0, 200.0);
        assert!((z - 0.5).abs() < 1e-9);
        // At scale factor 2, a 400px texture fits a 200-logical window.
        st.rotation = 0;
        let z = effective_zoom(&st, 200.0, 200.0, 2.0, 400.0, 400.0);
        assert_eq!(z, 1.0);
    }

    #[test]
    fn offset_clamping() {
        // Image fits: always centered.
        assert_eq!(
            clamp_offset((50.0, -50.0), 100.0, 100.0, 400.0, 400.0),
            (0.0, 0.0)
        );
        // Image overflows: clamped to edges.
        let (ox, oy) = clamp_offset((500.0, -500.0), 800.0, 800.0, 400.0, 400.0);
        assert_eq!((ox, oy), (200.0, -200.0));
    }

    #[test]
    fn markup_points_follow_every_view_rotation() {
        let transform = |rotation| ViewTransform {
            source: (100.0, 50.0),
            displayed: (100.0, 50.0),
            center: (200.0, 100.0),
            rotation,
        };
        for rotation in 0..4 {
            assert_eq!(
                transform(rotation).source_point((200.0, 100.0), false),
                Some(Point::new(50.0, 25.0)),
                "image centre at rotation {rotation}"
            );
        }
        for (rotation, widget_top_left) in [
            (0, (150.0, 75.0)),
            (1, (225.0, 50.0)),
            (2, (250.0, 125.0)),
            (3, (175.0, 150.0)),
        ] {
            assert_eq!(
                transform(rotation).source_point(widget_top_left, false),
                Some(Point::new(0.0, 0.0)),
                "source top-left at rotation {rotation}"
            );
        }
    }

    #[test]
    fn markup_drag_must_start_inside_but_endpoint_clamps() {
        let transform = ViewTransform {
            source: (100.0, 50.0),
            displayed: (50.0, 25.0),
            center: (200.0, 100.0),
            rotation: 0,
        };
        assert_eq!(transform.source_point((100.0, 20.0), false), None);
        assert_eq!(
            transform.source_point((100.0, 20.0), true),
            Some(Point::new(0.0, 0.0))
        );
        assert_eq!(transform.source_distance(3.0), 6.0);
    }
}
