"""A displayed negative control for the VM's keyboard and pointer route."""

import gi

gi.require_version('Gtk', '4.0')
gi.require_version('Gdk', '4.0')
gi.require_foreign('cairo')
from gi.repository import Gdk, Gtk


def activate(app):
    print(f"[input-probe] double-click-time={Gtk.Settings.get_default().get_property('gtk-double-click-time')}",
          flush=True)
    window = Gtk.ApplicationWindow(application=app, title='CI input probe')
    window.set_default_size(640, 480)
    area = Gtk.DrawingArea()
    color = [0.86, 0.12, 0.12]
    area.set_draw_func(lambda _area, cr, _w, _h: (cr.set_source_rgb(*color), cr.paint()))
    window.set_child(area)

    def key(_controller, keyval, _keycode, _state):
        print(f'[input-probe] key={keyval}', flush=True)
        if keyval == Gdk.KEY_Right:
            color[:] = [0.12, 0.86, 0.12]
            area.queue_draw()
        if keyval == Gdk.KEY_Escape:
            window.unfullscreen()
        return False

    keyboard = Gtk.EventControllerKey()
    keyboard.connect('key-pressed', key)
    window.add_controller(keyboard)
    pointer = Gtk.GestureClick()
    def pressed(gesture, count, x, y):
        event = gesture.get_current_event()
        device = event.get_device()
        print(f'[input-probe] click={count} x={x} y={y} time={event.get_time()} '
              f'device={device.get_name()} source={device.get_source()}', flush=True)
        if count == 2:
            print('[input-probe] fullscreen requested', flush=True)
            window.fullscreen()

    pointer.connect('pressed', pressed)
    area.add_controller(pointer)
    window.present()


app = Gtk.Application(application_id='io.github.TheRealShek.OpenMpv.CIProbe')
app.connect('activate', activate)
app.run(None)
