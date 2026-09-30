import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const ACTIONS = {
    'toggle-shortcut': 'toggle',
    'start-shortcut': 'start',
    'stop-shortcut': 'stop',
    'cancel-shortcut': 'cancel',
    'copy-shortcut': 'copy-last',
    'paste-shortcut': 'paste-last',
};
const CONTEXT_XML = `<node><interface name="org.xflow.Shell">
    <method name="Context"><arg name="json" type="s" direction="out"/></method>
</interface></node>`;

export default class XFlowExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._enabled = true;
        this._state = 'idle';
        this._lastFrame = 0;
        this._hideSource = 0;
        this._children = new Set();
        this._pill = new St.BoxLayout({style_class: 'xflow-pill', reactive: false, visible: false});
        this._label = new St.Label({text: 'XFlow', y_align: Clutter.ActorAlign.CENTER});
        this._wave = new St.BoxLayout({style_class: 'xflow-wave', y_align: Clutter.ActorAlign.CENTER});
        this._bars = Array.from({length: 9}, () => {
            const bar = new St.Widget({style_class: 'xflow-bar', width: 3, height: 3, y_align: Clutter.ActorAlign.CENTER});
            this._wave.add_child(bar);
            return bar;
        });
        this._pill.add_child(this._wave);
        this._pill.add_child(this._label);
        Main.layoutManager.addChrome(this._pill, {affectsInputRegion: false, trackFullscreen: false});
        this._settingsSignal = this._settings.connect('changed', (_settings, key) => {
            if (Object.hasOwn(ACTIONS, key)) this._bind(key, ACTIONS[key]);
            else this._configure();
        });
        this._monitorSignal = Main.layoutManager.connect('monitors-changed', () => this._position());
        for (const [name, action] of Object.entries(ACTIONS)) this._bind(name, action);
        this._configure();
        // Return only app/window identity. No surrounding text is read.
        this._context = Gio.DBusExportedObject.wrapJSObject(CONTEXT_XML, {
            Context: () => {
                if (Main.sessionMode.isLocked)
                    return JSON.stringify({app_id: null, window_id: null, selected_text: null});
                const window = global.display.focus_window;
                const app = window ? Shell.WindowTracker.get_default().get_window_app(window) : null;
                return JSON.stringify({
                    app_id: app?.get_id() ?? window?.get_wm_class() ?? null,
                    window_id: window ? String(window.get_stable_sequence()) : null,
                    selected_text: null,
                });
            },
        });
        this._context.export(Gio.DBus.session, '/org/xflow/Shell');
        this._busOwner = Gio.bus_own_name_on_connection(Gio.DBus.session, 'org.xflow.Shell', Gio.BusNameOwnerFlags.NONE, null, null);
        this._subscription = Gio.DBus.session.signal_subscribe(
            'org.xflow.Daemon', 'org.xflow.Daemon', 'Event', '/org/xflow/Daemon', null,
            Gio.DBusSignalFlags.NONE, (_connection, _sender, _path, _interface, _signal, parameters) => {
                try { this._update(JSON.parse(parameters.deep_unpack()[0])); }
                catch { this._update({state: 'error', message: 'Invalid daemon event'}); }
            });
        this._daemonWatch = Gio.bus_watch_name_on_connection(Gio.DBus.session, 'org.xflow.Daemon', Gio.BusNameWatcherFlags.NONE,
            null, () => this._update({state: 'idle'}));
    }

    _bind(name, action) {
        Main.wm.removeKeybinding(name);
        Main.wm.addKeybinding(name, this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT ?? Meta.KeyBindingFlags.NONE,
            Shell.ActionMode.NORMAL, () => this._command(action));
    }

    _configure() {
        this._pill.set_width(this._settings.get_int('width'));
        this._pill.set_height(this._settings.get_int('height'));
        this._pill.opacity = Math.round(this._settings.get_double('opacity') * 255);
        this._position();
    }

    _position() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor) return;
        const width = this._settings.get_int('width');
        const height = this._settings.get_int('height');
        const margin = this._settings.get_int('margin');
        const position = this._settings.get_string('position');
        this._pill.set_position(
            monitor.x + Math.round((monitor.width - width) / 2),
            position === 'top' ? monitor.y + Main.panel.height + margin : monitor.y + monitor.height - height - margin);
    }

    _command(action) {
        // Explicit argv: the user-configured path and transcript never become shell code.
        try {
            const child = Gio.Subprocess.new([this._settings.get_string('command-path'), action],
                Gio.SubprocessFlags.STDOUT_SILENCE | Gio.SubprocessFlags.STDERR_SILENCE);
            this._children.add(child);
            child.wait_check_async(null, (process, result) => {
                this._children.delete(process);
                if (!this._enabled) return;
                try { process.wait_check_finish(result); }
                catch { this._update({state: 'error', message: 'XFlow command failed; check daemon'}); }
            });
        } catch {
            this._update({state: 'error', message: 'XFlow executable unavailable'});
        }
    }

    _update(event) {
        if (!this._enabled) return;
        if (!['idle', 'listening', 'processing', 'success', 'error'].includes(event.state)) return;
        const changed = this._state !== event.state;
        this._state = event.state;
        if (this._hideSource) {
            GLib.Source.remove(this._hideSource);
            this._hideSource = 0;
        }
        if (event.state === 'idle') { this._pill.hide(); return; }
        this._pill.show();
        this._wave.visible = event.state === 'listening';
        const labels = {listening: 'Listening', processing: 'Transcribing…', success: 'Ready', error: 'XFlow error'};
        this._label.text = String(event.message || labels[event.state]).slice(0, 80);
        this._pill.set_style(`border-color: ${event.state === 'error' ? '#f66151' : event.state === 'success' ? '#57e389' : '#78aeed'};`);
        // Event-driven waveform, capped at 30 updates/second. No recurring timer,
        // idle animation, audio polling, or transcript text rendered in the pill.
        const now = GLib.get_monotonic_time();
        if (event.state === 'listening' && (changed || now - this._lastFrame >= 33333)) {
            this._lastFrame = now;
            const level = Number.isFinite(event.level) ? Math.max(0, Math.min(1, event.level * 8)) : 0;
            const animated = this._settings.get_boolean('animation');
            const maxHeight = Math.max(3, this._settings.get_int('height') - 16);
            this._bars.forEach((bar, index) => {
                const envelope = 1 - Math.abs(index - 4) / 6;
                const height = Math.max(3, Math.round(maxHeight * level * envelope));
                if (animated) bar.ease({height, duration: 65, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
                else { bar.remove_all_transitions(); bar.height = height; }
            });
        }
        if (event.state === 'success' || event.state === 'error') {
            this._hideSource = GLib.timeout_add(GLib.PRIORITY_DEFAULT, event.state === 'error' ? 4000 : 1500, () => {
                this._hideSource = 0;
                this._pill.hide();
                return GLib.SOURCE_REMOVE;
            });
        }
    }

    disable() {
        this._enabled = false;
        for (const name of Object.keys(ACTIONS)) Main.wm.removeKeybinding(name);
        if (this._subscription) Gio.DBus.session.signal_unsubscribe(this._subscription);
        if (this._daemonWatch) Gio.bus_unwatch_name(this._daemonWatch);
        if (this._context) this._context.unexport();
        if (this._busOwner) Gio.bus_unown_name(this._busOwner);
        if (this._hideSource) GLib.Source.remove(this._hideSource);
        if (this._monitorSignal) Main.layoutManager.disconnect(this._monitorSignal);
        if (this._settingsSignal) this._settings.disconnect(this._settingsSignal);
        for (const child of this._children ?? []) child.force_exit();
        this._children?.clear();
        this._pill?.destroy();
        this._pill = null;
        this._context = null;
        this._settings = null;
    }
}
