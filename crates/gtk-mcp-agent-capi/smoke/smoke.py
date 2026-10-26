"""Python smoke app: attaches the gtk-mcp agent through the GtkMcp typelib."""
import gi

gi.require_version("Gtk", "4.0")
gi.require_version("GtkMcp", "1")
from gi.repository import Gio, Gtk, GtkMcp

TOKEN = "py-hunter2-TOKEN-0xC0FFEE"


def on_activate(app):
    win = Gtk.ApplicationWindow(application=app)
    panel = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
    panel.set_name("smoke-panel")
    label = Gtk.Label(label="hello from python")
    label.set_name("smoke-label")
    secret = Gtk.Entry()
    secret.set_name("smoke-secret")
    secret.set_text(TOKEN)
    GtkMcp.mark_secret(secret)
    group = Gio.SimpleActionGroup()
    group.add_action(Gio.SimpleAction.new("ping", None))
    panel.insert_action_group("smoke", group)
    GtkMcp.register_action_group(panel, "smoke", group)
    panel.append(label)
    panel.append(secret)
    win.set_child(panel)
    win.present()


app = Gtk.Application(application_id="dev.gtkmcp.SmokePy")
app.connect("activate", on_activate)
GtkMcp.attach(app)
GtkMcp.register_state("smoke", lambda: '{"lang":"python","tag":"one"}')
app.run(None)
