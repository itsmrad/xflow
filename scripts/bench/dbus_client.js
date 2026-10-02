// GJS/Gio command RTT on the harness's private bus; never a GNOME Shell instance.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const address = GLib.getenv('DBUS_SESSION_BUS_ADDRESS') || '';
if (!GLib.getenv('XFLOW_BENCH_ISOLATED_BUS') || !address || address.includes('/run/user/'))
    throw new Error('Private benchmark bus required');
const count = Number(ARGV[0] || 200);
if (!Number.isSafeInteger(count) || count < 1)
    throw new Error('Positive sample count required');
const connection = Gio.DBusConnection.new_for_address_sync(address,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    null, null);
const request = JSON.stringify({command: 'toggle', mode: 'dictation', context:
    {app_id: 'org.gnome.TextEditor.desktop', window_id: '42', selected_text: null}, t0_us: 123456789});
const samples = [];
for (let i = 0; i < count; i++) {
    const started = GLib.get_monotonic_time();
    const response = connection.call_sync('org.xflow.Daemon', '/org/xflow/Daemon',
        'org.xflow.Daemon', 'Command', new GLib.Variant('(s)', [request]),
        new GLib.VariantType('(s)'), Gio.DBusCallFlags.NONE, 5000, null);
    const [json] = response.deep_unpack();
    if (!JSON.parse(json).ok)
        throw new Error('Stub command failed');
    samples.push(GLib.get_monotonic_time() - started);
}
connection.close_sync(null);
print(JSON.stringify({samples}));
