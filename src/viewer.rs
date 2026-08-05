//! Image display widget (FR-4): fit/actual/manual zoom, cursor-anchored
//! zoom, pan with edge clamping, 90° view rotation, and physical-pixel
//! alignment so 100% is pixel-exact under fractional scaling (FR-4.7).
//!
//! Zoom is expressed against physical pixels: zoom 1.0 means one texture
//! pixel maps to one physical display pixel regardless of scale factor.

use gtk4 as gtk;

use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

pub const ZOOM_MIN: f64 = 0.05;
pub const ZOOM_MAX: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Scale to the window, never above 100% (FR-4.1). Not pannable.
    Fit,
    /// Explicit zoom level; 1.0 is pixel-exact 100%.
    Manual(f64),
}

pub struct State {
    texture: Option<gtk::gdk::Texture>,
    /// Logical pixel size of the image independent of texture resolution.
    /// For raster images this equals the texture size; for SVG it is the
    /// document's nominal size, so re-rendered hi-res textures keep the
    /// same on-screen geometry (FR-2.3).
    nominal: Option<(f64, f64)>,
    mode: Mode,
    /// Pan offset of the image center from the widget center, logical px.
    offset: (f64, f64),
    /// View rotation in quarter turns, 0..4.
    rotation: u8,
    default_fit_actual: bool,
    pointer: (f64, f64),
    drag_origin: (f64, f64),
}

impl Default for State {
    fn default() -> Self {
        State {
            texture: None,
            nominal: None,
            mode: Mode::Fit,
            offset: (0.0, 0.0),
            rotation: 0,
            default_fit_actual: false,
            pointer: (0.0, 0.0),
            drag_origin: (0.0, 0.0),
        }
    }
}

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct ImageView {
        pub(super) state: RefCell<State>,
        /// Fired when zoom/rotation changes; argument is zoom percent.
        pub(super) on_view_changed: RefCell<Option<Box<dyn Fn(f64)>>>,
        /// Fired on horizontal scroll: +1 next, -1 prev (FR-3.1).
        pub(super) on_navigate: RefCell<Option<Box<dyn Fn(i32)>>>,
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
            let state = self.state.borrow();
            let Some(texture) = state.texture.as_ref() else {
                return;
            };
            let (w, h) = (obj.width() as f64, obj.height() as f64);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let scale = obj.surface_scale();
            let (tw, th) = super::image_dims(&state, texture);
            let zoom = super::effective_zoom(&state, w, h, scale, tw, th);
            // Displayed size in logical px (unrotated and rotated).
            let (dw, dh) = (tw * zoom / scale, th * zoom / scale);
            let (rw, rh) = if state.rotation % 2 == 1 {
                (dh, dw)
            } else {
                (dw, dh)
            };
            let offset = super::clamp_offset(state.offset, rw, rh, w, h);

            // Snap the image center to the physical pixel grid so a
            // 100% view is never compositor-blurred (FR-4.7).
            let cx = ((w / 2.0 + offset.0) * scale).round() / scale;
            let cy = ((h / 2.0 + offset.1) * scale).round() / scale;

            let filter = if zoom < 1.0 {
                gsk::ScalingFilter::Trilinear
            } else if zoom > 1.0 {
                gsk::ScalingFilter::Linear
            } else {
                gsk::ScalingFilter::Nearest
            };

            snapshot.save();
            snapshot.translate(&graphene::Point::new(cx as f32, cy as f32));
            if state.rotation != 0 {
                snapshot.rotate(state.rotation as f32 * 90.0);
            }
            snapshot.append_scaled_texture(
                texture,
                filter,
                &graphene::Rect::new(
                    (-dw / 2.0) as f32,
                    (-dh / 2.0) as f32,
                    dw as f32,
                    dh as f32,
                ),
            );
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

fn image_dims(state: &State, texture: &gtk::gdk::Texture) -> (f64, f64) {
    state
        .nominal
        .unwrap_or((texture.width() as f64, texture.height() as f64))
}

fn effective_zoom(state: &State, w: f64, h: f64, scale: f64, tw: f64, th: f64) -> f64 {
    let (rtw, rth) = if state.rotation % 2 == 1 {
        (th, tw)
    } else {
        (tw, th)
    };
    match state.mode {
        Mode::Fit => (w * scale / rtw).min(h * scale / rth).min(1.0),
        Mode::Manual(z) => z,
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
        {
            let mut st = self.state();
            st.texture = Some(texture);
            st.nominal = nominal;
            st.rotation = 0;
            st.offset = (0.0, 0.0);
            st.mode = if st.default_fit_actual {
                Mode::Manual(1.0)
            } else {
                Mode::Fit
            };
        }
        self.queue_draw();
        self.emit_view_changed();
    }

    /// Remove the image (empty and error states).
    pub fn clear(&self) {
        self.state().texture = None;
        self.queue_draw();
    }

    /// Swap the texture without touching view state (animation frames,
    /// SVG re-render).
    pub fn update_texture(&self, texture: gtk::gdk::Texture) {
        self.state().texture = Some(texture);
        self.queue_draw();
    }

    pub fn texture(&self) -> Option<gtk::gdk::Texture> {
        self.imp().state.borrow().texture.clone()
    }

    /// Initial fit mode from config (FR-8.2 `fit=`).
    pub fn set_default_fit_actual(&self, actual: bool) {
        self.state().default_fit_actual = actual;
    }

    pub fn zoom_percent(&self) -> f64 {
        let st = self.imp().state.borrow();
        let Some(tex) = st.texture.as_ref() else {
            return 100.0;
        };
        let (tw, th) = image_dims(&st, tex);
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

    pub fn is_pannable(&self) -> bool {
        let st = self.imp().state.borrow();
        let Some(tex) = st.texture.as_ref() else {
            return false;
        };
        let (w, h) = (self.width() as f64, self.height() as f64);
        let scale = self.surface_scale();
        let (tw, th) = image_dims(&st, tex);
        let z = effective_zoom(&st, w, h, scale, tw, th);
        let (dw, dh) = (tw * z / scale, th * z / scale);
        let (rw, rh) = if st.rotation % 2 == 1 { (dh, dw) } else { (dw, dh) };
        rw > w + 0.5 || rh > h + 0.5
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

    pub fn connect_navigate(&self, f: impl Fn(i32) + 'static) {
        *self.imp().on_navigate.borrow_mut() = Some(Box::new(f));
    }

    fn emit_view_changed(&self) {
        let percent = self.zoom_percent();
        if let Some(f) = self.imp().on_view_changed.borrow().as_ref() {
            f(percent);
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
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, dx, dy| {
                if dx.abs() > dy.abs() {
                    if let Some(f) = view.imp().on_navigate.borrow().as_ref() {
                        f(if dx > 0.0 { 1 } else { -1 });
                    }
                } else if dy != 0.0 {
                    let anchor = view.imp().state.borrow().pointer;
                    view.zoom_by(1.1f64.powf(-dy), Some(anchor));
                }
                glib::Propagation::Stop
            }
        ));
        self.add_controller(scroll);

        // Drag pans when the image overflows the viewport; otherwise the
        // gesture is denied so the window's drag-to-move can take over
        // (FR-6.4).
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, _, _| {
                if view.is_pannable() {
                    view.state().drag_origin = view.imp().state.borrow().offset;
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
                let anchor = gesture.bounding_box_center().map(|(x, y)| (x, y));
                view.zoom_to(base.get() * scale, anchor);
            }
        ));
        self.add_controller(pinch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_upscales() {
        let st = State::default();
        // 100x100 texture in a 400x400 logical window at scale 1.
        assert_eq!(effective_zoom(&st, 400.0, 400.0, 1.0, 100.0, 100.0), 1.0);
        // Large texture scales down.
        let z = effective_zoom(&st, 400.0, 400.0, 1.0, 800.0, 400.0);
        assert!((z - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fit_accounts_for_rotation_and_scale_factor() {
        let mut st = State::default();
        st.rotation = 1;
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
        assert_eq!(clamp_offset((50.0, -50.0), 100.0, 100.0, 400.0, 400.0), (0.0, 0.0));
        // Image overflows: clamped to edges.
        let (ox, oy) = clamp_offset((500.0, -500.0), 800.0, 800.0, 400.0, 400.0);
        assert_eq!((ox, oy), (200.0, -200.0));
    }
}
