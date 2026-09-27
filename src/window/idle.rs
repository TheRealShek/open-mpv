//! Keep the visible video surface awake on Wayland. GTK's session inhibit
//! does not reach compositors whose idle service listens to Wayland's
//! idle-inhibit protocol, so GTK remains the fallback when that protocol is
//! unavailable.

use gdk4_wayland::{WaylandDisplay, WaylandSurface, prelude::*};
use gtk4::prelude::*;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, protocol::wl_registry,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};

#[derive(Default)]
struct State {
    manager_name: Option<u32>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "zwp_idle_inhibit_manager_v1"
        {
            state.manager_name = Some(name);
        }
    }
}

delegate_noop!(State: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(State: ignore ZwpIdleInhibitorV1);

pub(super) struct WaylandIdle {
    connection: Connection,
    queue: wayland_client::EventQueue<State>,
    state: State,
    registry: wl_registry::WlRegistry,
    manager: Option<ZwpIdleInhibitManagerV1>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
}

impl WaylandIdle {
    pub(super) fn new(window: &gtk4::ApplicationWindow) -> Result<Option<Self>, String> {
        let Some(surface) = window.surface() else {
            return Ok(None);
        };
        let Ok(display) = surface.display().downcast::<WaylandDisplay>() else {
            return Ok(None);
        };
        if !display.query_registry("zwp_idle_inhibit_manager_v1") {
            return Ok(None);
        }
        let Some(wl_display) = display.wl_display() else {
            return Ok(None);
        };
        let backend = wl_display
            .backend()
            .upgrade()
            .ok_or("Wayland display connection closed")?;
        let connection = Connection::from_backend(backend);
        let queue = connection.new_event_queue();
        let registry = wl_display.get_registry(&queue.handle(), ());
        connection
            .flush()
            .map_err(|error| format!("cannot query Wayland idle manager: {error}"))?;
        Ok(Some(Self {
            connection,
            queue,
            state: State::default(),
            registry,
            manager: None,
            inhibitor: None,
        }))
    }

    pub(super) fn is_active(&self) -> bool {
        self.inhibitor.is_some()
    }

    /// Processes only events already read by GDK; GTK never waits for a reply.
    pub(super) fn inhibit(&mut self, window: &gtk4::ApplicationWindow) -> Result<bool, String> {
        if self.inhibitor.is_some() {
            return Ok(true);
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("cannot read Wayland idle registry: {error}"))?;
        if self.manager.is_none() {
            let Some(name) = self.state.manager_name else {
                return Ok(false);
            };
            self.manager = Some(self.registry.bind::<ZwpIdleInhibitManagerV1, _, _>(
                name,
                1,
                &self.queue.handle(),
                (),
            ));
        }
        let surface = window
            .surface()
            .ok_or("window has no surface")?
            .downcast::<WaylandSurface>()
            .map_err(|_| "window surface is not Wayland")?;
        let wl_surface = surface.wl_surface().ok_or("Wayland surface is not ready")?;
        let inhibitor = self
            .manager
            .as_ref()
            .expect("manager bound above")
            .create_inhibitor(&wl_surface, &self.queue.handle(), ());
        self.inhibitor = Some(inhibitor);
        if let Err(error) = self.connection.flush() {
            self.release();
            return Err(format!("cannot activate Wayland idle inhibition: {error}"));
        }
        Ok(true)
    }

    pub(super) fn release(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
            if let Err(error) = self.connection.flush() {
                crate::applog!("idle inhibit: cannot release Wayland inhibitor: {error}");
            }
        }
    }
}

impl Drop for WaylandIdle {
    fn drop(&mut self) {
        self.release();
        if let Some(manager) = self.manager.take() {
            manager.destroy();
        }
        let _ = self.connection.flush();
    }
}
