// GJS smoke app: attaches the gtk-mcp agent through the GtkMcp typelib.
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk?version=4.0';
import GtkMcp from 'gi://GtkMcp?version=1';

const TOKEN = 'js-hunter2-TOKEN-0xC0FFEE';

const app = new Gtk.Application({application_id: 'dev.gtkmcp.SmokeJs'});
app.connect('activate', () => {
    const win = new Gtk.ApplicationWindow({application: app});
    const panel = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 6});
    panel.set_name('smoke-panel');
    const label = new Gtk.Label({label: 'hello from gjs'});
    label.set_name('smoke-label');
    const secret = new Gtk.Entry();
    secret.set_name('smoke-secret');
    secret.set_text(TOKEN);
    GtkMcp.mark_secret(secret);
    const group = new Gio.SimpleActionGroup();
    group.add_action(new Gio.SimpleAction({name: 'ping'}));
    panel.insert_action_group('smoke', group);
    GtkMcp.register_action_group(panel, 'smoke', group);
    panel.append(label);
    panel.append(secret);
    win.set_child(panel);
    win.present();
});
GtkMcp.attach(app);
GtkMcp.register_state('smoke', () => '{"lang":"gjs","tag":"one"}');
app.run([]);
