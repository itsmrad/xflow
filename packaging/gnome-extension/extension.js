import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Pango from 'gi://Pango';
import Shell from 'gi://Shell';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Config from 'resource:///org/gnome/shell/misc/config.js';
import * as L from './logic.js';

const SHORTCUTS = {
    'toggle-shortcut': 'dictate', 'command-shortcut': 'command',
    'start-shortcut': 'start', 'stop-shortcut': 'stop', 'cancel-shortcut': 'cancel',
    'copy-shortcut': 'copy_last', 'paste-shortcut': 'paste_last',
};
const SHELL_XML = `<node><interface name="org.xflow.Shell">
    <method name="Context"><arg type="s" direction="out"/></method>
    <method name="Inject"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="Selection"><arg type="s" direction="out"/></method>
    <method name="Version"><arg type="s" direction="out"/></method>
</interface></node>`;
const ACTIVE = new Set(['listening', 'processing']);

export default class XFlowExtension extends Extension {
    enable() {
        this._enabled = true;
        this._state = 'idle';
        this._mode = 'dictation';
        this._message = null;
        this._sources = new Set();
        this._connections = [];
        this._bindings = new Map();
        this._grabs = new Map();
        this._frameSource = this._holdSource = this._releaseSource = this._restoreSource = 0;
        this._revision = 0;
        this._queue = Promise.resolve();
        this._cancellable = new Gio.Cancellable();
        this._settings = this.getSettings();
        this._desktop = new Gio.Settings({schema_id: 'org.gnome.desktop.interface'});
        this._themeContext = St.ThemeContext.get_for_stage(global.stage);
        this._hotkey = new L.HotkeyMachine();
        this._commandHotkey = new L.HotkeyMachine();
        this._clipboard = St.Clipboard.get_default();
        this._pill = new St.BoxLayout({style_class: 'xflow-pill', reactive: false, visible: false,
            accessible_name: 'XFlow dictation'});
        this._pill.set_pivot_point(0.5, 0.5);
        this._wave = new St.BoxLayout({style_class: 'xflow-wave', y_align: Clutter.ActorAlign.CENTER});
        this._icon = new St.Icon({y_align: Clutter.ActorAlign.CENTER});
        this._label = new St.Label({y_align: Clutter.ActorAlign.CENTER});
        this._label.clutter_text.ellipsize = Pango.EllipsizeMode.END;
        this._pill.add_child(this._wave);
        this._pill.add_child(this._icon);
        this._pill.add_child(this._label);
        const chrome = {trackFullscreen: false};
        if (Number.parseInt(Config.PACKAGE_VERSION, 10) < 50) chrome.affectsInputRegion = false;
        Main.layoutManager.addChrome(this._pill, chrome);
        this._connect(this._settings, 'changed', (_settings, key) => {
            if (Object.hasOwn(SHORTCUTS, key)) this._bind(key);
            else {
                if (key === 'hotkey-mode' || key === 'ptt-threshold-ms') this._resetHeld();
                this._configure();
            }
        });
        this._connect(this._desktop, 'changed::color-scheme', () => this._render());
        this._connect(this._desktop, 'changed::enable-animations', () => this._render());
        this._connect(this._themeContext, 'notify::scale-factor', () => this._configure());
        this._connect(Main.layoutManager, 'monitors-changed', () => this._position());
        this._connect(global.display, 'workareas-changed', () => this._position());
        this._connect(global.display, 'notify::focus-window', () => this._position());
        this._connect(Main.sessionMode, 'updated', () => {
            if (Main.sessionMode.isLocked) {
                this._resetHeld();
                this._cancelSource('_restoreSource');
                this._update({state: 'idle'});
            }
        });
        this._connect(global.display, 'accelerator-activated', (_display, action) => this._activate(action));
        this._connect(global.display, 'accelerator-deactivated', (_display, action) => this._release(action));
        this._configure();
        for (const key of Object.keys(SHORTCUTS)) this._bind(key);
        this._shell = Gio.DBusExportedObject.wrapJSObject(SHELL_XML, {
            Context: () => JSON.stringify(this._context()),
            Version: () => String(this.metadata.version),
            SelectionAsync: (_args, invocation) => {
                if (Main.sessionMode.isLocked) {
                    invocation.return_dbus_error('org.xflow.Shell.AccessDenied', 'Screen is locked');
                    return;
                }
                this._getClipboard(St.ClipboardType.PRIMARY).then(text => {
                    this._returnJson(invocation, {text: text === null ? null : L.truncateUtf8(text, L.MAX_SELECTION_BYTES)});
                }).catch(() => invocation.return_dbus_error('org.xflow.Shell.Failed', 'Selection unavailable'));
            },
            InjectAsync: ([text, optionsJson], invocation) => {
                if (Main.sessionMode.isLocked) {
                    invocation.return_dbus_error('org.xflow.Shell.AccessDenied', 'Screen is locked');
                    return;
                }
                let options;
                try {
                    options = L.parseInjectOptions(optionsJson);
                    if (L.utf8Length(text) > L.MAX_INJECT_BYTES) throw new Error();
                } catch {
                    invocation.return_dbus_error('org.xflow.Shell.InvalidArgs', 'Invalid injection request');
                    return;
                }
                // Serialize clipboard ownership/restoration as well as key events.
                this._injectQueue = (this._injectQueue ?? Promise.resolve()).then(() => this._inject(text, options));
                this._injectQueue.then(result => this._returnJson(invocation, result)).catch(() => {
                    invocation.return_dbus_error('org.xflow.Shell.Failed', 'Desktop injection unavailable');
                });
                this._injectQueue = this._injectQueue.catch(() => {});
            },
        });
        this._shell.export(Gio.DBus.session, '/org/xflow/Shell');
        this._busOwner = Gio.bus_own_name_on_connection(Gio.DBus.session, 'org.xflow.Shell', Gio.BusNameOwnerFlags.NONE, null, null);
        this._subscription = Gio.DBus.session.signal_subscribe('org.xflow.Daemon', 'org.xflow.Daemon', 'Event',
            '/org/xflow/Daemon', null, Gio.DBusSignalFlags.NONE,
            (_connection, _sender, _path, _iface, _signal, parameters) => {
                const event = L.parseEvent(parameters.deep_unpack()[0]);
                if (event) this._update(event);
            });
        this._hasDaemon = false;
        this._daemonWatch = Gio.bus_watch_name_on_connection(Gio.DBus.session, 'org.xflow.Daemon',
            Gio.BusNameWatcherFlags.NONE, () => {
                this._hasDaemon = true;
                this._command('status');
            }, () => {
                this._hasDaemon = false;
                if (ACTIVE.has(this._state)) this._update({state: 'error', message: L.NOT_RUNNING});
            });
    }

    _connect(object, signal, callback) {
        this._connections.push([object, object.connect(signal, callback)]);
    }

    _timeout(ms, callback, repeat = false) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            if (!this._enabled) { this._sources.delete(id); return GLib.SOURCE_REMOVE; }
            const again = callback();
            if (repeat && again !== false) return GLib.SOURCE_CONTINUE;
            this._sources.delete(id);
            return GLib.SOURCE_REMOVE;
        });
        this._sources.add(id);
        return id;
    }

    _cancelSource(field) {
        const id = this[field];
        if (id && this._sources.delete(id)) GLib.Source.remove(id);
        this[field] = 0;
    }

    _animations() {
        return this._settings.get_boolean('animations') && this._desktop.get_boolean('enable-animations');
    }

    _configure() {
        const s = this._settings;
        this._geometry = L.pillGeometry({barCount: s.get_int('waveform-bars'),
            barStyle: s.get_string('waveform-style'), scale: s.get_double('scale')});
        this._hotkey.configure(s.get_string('hotkey-mode'), s.get_int('ptt-threshold-ms'));
        this._commandHotkey.configure(s.get_string('hotkey-mode'), s.get_int('ptt-threshold-ms'));
        for (const child of this._wave.get_children()) child.destroy();
        const g = this._geometry;
        this._bars = [];
        for (let i = 0; i < g.count; i++) {
            const bar = new St.Widget({style_class: 'xflow-bar', y_align: Clutter.ActorAlign.CENTER,
                style: `width: ${g.barWidth}px; border-radius: ${g.barRadius}px;`});
            this._wave.add_child(bar);
            this._bars.push(bar);
        }
        this._wave.set_style(`spacing: ${g.barGap}px;`);
        this._waveform = new L.Waveform(g.count);
        this._processingWaveform = new L.Waveform(5);
        this._render();
        this._scheduleHold();
    }

    _render() {
        if (!this._enabled) return;
        const s = this._settings;
        const g = this._geometry;
        const idle = this._state === 'idle';
        const minimal = idle && s.get_string('idle-style') === 'minimal';
        const theme = L.resolveTheme(s.get_string('theme'), this._desktop.get_string('color-scheme'));
        this._pill.style_class = `xflow-pill xflow-${theme} xflow-${this._state}${this._mode === 'command' ? ' xflow-command' : ''}`;
        this._pill.remove_all_transitions();
        const sf = this._themeContext.scale_factor;
        this._pill.set_style(`padding: ${minimal ? 0 : 8 * s.get_double('scale')}px ${minimal ? 0 : g.padX}px; spacing: ${g.spacing}px; max-width: ${g.maxWidth}px; border-radius: ${g.height / 2}px;`);
        this._pill.height = (minimal ? g.handleHeight : g.height) * sf;
        this._pill.width = minimal ? g.handleWidth * sf : -1;
        this._wave.visible = ACTIVE.has(this._state);
        for (let i = 0; i < this._bars.length; i++) this._bars[i].visible = this._state !== 'processing' || i < 5;
        this._icon.visible = this._state === 'success' || this._state === 'error';
        this._icon.icon_name = this._state === 'error' ? 'dialog-warning-symbolic' : 'object-select-symbolic';
        this._icon.icon_size = g.iconSize;
        const labels = {listening: this._mode === 'command' ? 'Command' : 'Listening', processing: 'Processing', success: 'Done', error: 'XFlow error'};
        this._label.text = this._message ?? labels[this._state] ?? '';
        this._label.set_style(`font-size: ${g.fontSize}px; max-width: ${Math.max(80, g.maxWidth - g.width - 40)}px;`);
        this._label.visible = !idle && s.get_boolean('show-messages');
        this._pill.accessible_name = idle ? 'XFlow ready' : `XFlow ${labels[this._state]}`;
        const opacity = Math.round(s.get_double('opacity') * 255);
        if (idle && !minimal) {
            this._cancelSource('_frameSource');
            if (this._pill.visible && this._animations()) {
                this._pill.ease({opacity: 0, scale_x: 0.65, scale_y: 0.8, duration: 140,
                    mode: Clutter.AnimationMode.EASE_OUT_QUAD,
                    onComplete: () => { if (this._enabled && this._state === 'idle') this._pill.hide(); }});
            } else this._pill.hide();
        } else {
            const appearing = !this._pill.visible;
            this._pill.show();
            if (appearing && this._animations()) {
                this._pill.opacity = 0;
                this._pill.scale_x = 0.65;
                this._pill.scale_y = 0.8;
            }
            if (this._animations()) this._pill.ease({opacity, scale_x: 1, scale_y: 1, duration: 160,
                mode: Clutter.AnimationMode.EASE_OUT_QUAD});
            else { this._pill.opacity = opacity; this._pill.scale_x = this._pill.scale_y = 1; }
        }
        this._position();
        this._syncFrames();
    }

    _position() {
        if (!this._enabled || !Main.layoutManager.monitors.length) return;
        const [px, py] = global.get_pointer();
        const monitors = Main.layoutManager.monitors;
        const index = L.pickMonitor(this._settings.get_string('monitor'), {
            primary: Main.layoutManager.primaryIndex, focused: global.display.focus_window?.get_monitor(),
            pointer: monitors.findIndex(m => px >= m.x && px < m.x + m.width && py >= m.y && py < m.y + m.height), count: monitors.length,
        });
        const area = global.workspace_manager.get_active_workspace().get_work_area_for_monitor(index);
        const [, naturalWidth] = this._pill.get_preferred_width(-1);
        const sf = this._themeContext.scale_factor;
        const pos = L.pillPosition(area, Math.min(naturalWidth, area.width), this._pill.height,
            this._settings.get_string('position'), this._settings.get_int('margin') * sf,
            this._settings.get_int('offset-x') * sf, this._settings.get_int('offset-y') * sf);
        this._pill.set_position(pos.x, pos.y);
    }

    _syncFrames() {
        const animate = ACTIVE.has(this._state) && this._animations();
        if (!animate) this._cancelSource('_frameSource');
        this._drawFrame(0);
        if (animate && !this._frameSource) {
            this._lastFrame = GLib.get_monotonic_time() / 1000;
            this._frameSource = this._timeout(33, () => {
                const now = GLib.get_monotonic_time() / 1000;
                this._drawFrame(now - this._lastFrame, now);
                this._lastFrame = now;
            }, true);
        }
    }

    _drawFrame(dt, now = GLib.get_monotonic_time() / 1000) {
        if (!ACTIVE.has(this._state)) return;
        const waveform = this._state === 'processing' ? this._processingWaveform : this._waveform;
        const heights = waveform.step(dt, now, this._state, this._animations());
        const g = this._geometry;
        const sf = this._themeContext.scale_factor;
        for (let i = 0; i < Math.min(this._bars.length, waveform.count); i++) {
            this._bars[i].height = Math.round((g.barMin + (g.barMax - g.barMin) * heights[i]) * sf);
            this._bars[i].opacity = Math.round(waveform.opacity[i] * 255);
        }
    }

    _update(event) {
        if (!this._enabled || Main.sessionMode.isLocked && event.state !== 'idle') return;
        ++this._revision;
        const changed = this._state !== event.state || this._mode !== (event.mode ?? this._mode)
            || this._message !== (event.message ?? null);
        this._state = event.state;
        this._mode = event.mode ?? this._mode;
        this._message = event.message ?? null;
        if (event.state === 'listening') this._waveform.setLevel(event.level ?? 0);
        if (changed) {
            this._cancelSource('_holdSource');
            if (!ACTIVE.has(this._state)) this._resetHeld();
            this._escape(ACTIVE.has(this._state));
            this._render();
            this._scheduleHold();
        } else if (!this._animations()) this._drawFrame(0);
    }

    _scheduleHold() {
        this._cancelSource('_holdSource');
        if (this._state !== 'success' && this._state !== 'error') return;
        const ms = L.holdTime(this._state, {successMs: this._settings.get_int('success-hold-ms'), errorMs: this._settings.get_int('error-hold-ms')});
        this._holdSource = this._timeout(Math.max(1, ms), () => {
            this._holdSource = 0;
            this._update({state: 'idle'});
        });
    }

    _bind(key) {
        for (const action of this._bindings.get(key) ?? []) {
            this._release(action);
            this._ungrab(action);
        }
        const actions = [];
        for (const accelerator of this._settings.get_strv(key)) {
            const action = this._grab(accelerator, SHORTCUTS[key]);
            if (action) actions.push(action);
        }
        this._bindings.set(key, actions);
    }

    _grab(accelerator, command) {
        const action = global.display.grab_accelerator(accelerator, Meta.KeyBindingFlags.IGNORE_AUTOREPEAT);
        if (action === Meta.KeyBindingAction.NONE) return 0;
        const name = Meta.external_binding_name_for_action(action);
        Main.wm.allowKeybinding(name, Shell.ActionMode.NORMAL);
        const hasModifiers = /<(?:Control|Ctrl|Primary|Alt|Shift|Super|Meta|Hyper|Mod[1-5])>/i.test(accelerator);
        this._grabs.set(action, {command, hasModifiers});
        return action;
    }

    _ungrab(action) {
        Main.wm.allowKeybinding(Meta.external_binding_name_for_action(action), Shell.ActionMode.NONE);
        global.display.ungrab_accelerator(action);
        this._grabs.delete(action);
    }

    _escape(active) {
        if (!active && this._escapeAction) { this._ungrab(this._escapeAction); this._escapeAction = 0; }
        if (active && !this._escapeAction) this._escapeAction = this._grab('Escape', 'cancel');
    }

    _activate(action) {
        const binding = this._grabs.get(action);
        if (!binding || Main.sessionMode.isLocked) return;
        if (binding.command === 'dictate' || binding.command === 'command') {
            if (this._state === 'processing') return;
            const machine = binding.command === 'command' ? this._commandHotkey : this._hotkey;
            const t0Us = GLib.get_monotonic_time();
            const command = machine.press(t0Us / 1000, this._state === 'listening');
            if (command) this._command(command, binding.command === 'command' ? 'command' : 'dictation', t0Us);
            if (machine.holding) {
                this._heldAction = action;
                this._heldMachine = machine;
                // Mutter's release signal handles the trigger key; modifier-first
                // release needs a watcher, running only for this held shortcut.
                this._cancelSource('_releaseSource');
                const mask = L.heldModifierMask(global.get_pointer()[2]);
                if (binding.hasModifiers) this._releaseSource = this._timeout(16, () => {
                    const [, , modifiers] = global.get_pointer();
                    if (!mask || (modifiers & mask) !== mask) {
                        this._releaseSource = 0;
                        this._release(action);
                        return false;
                    }
                }, true);
            }
        } else this._command(binding.command);
    }

    _release(action) {
        if (action !== this._heldAction) return;
        const command = this._heldMachine.release(GLib.get_monotonic_time() / 1000);
        this._cancelSource('_releaseSource');
        this._heldAction = 0;
        this._heldMachine = null;
        if (command) this._command(command);
    }

    _resetHeld() {
        this._cancelSource('_releaseSource');
        this._heldAction = 0;
        this._heldMachine = null;
        this._hotkey.reset();
        this._commandHotkey.reset();
    }

    _command(command, mode = this._mode, t0Us = GLib.get_monotonic_time()) {
        if (!this._enabled || Main.sessionMode.isLocked) return;
        const context = command === 'start' || command === 'toggle' ? this._context() : null;
        const state = L.optimisticState(command, this._state);
        if (state) this._update({state, mode, level: 0});
        const revision = this._revision;
        const request = L.commandRequest(command, {mode, context, t0Us});
        this._queue = this._queue.then(async () => {
            if (!this._enabled) return;
            if (!this._hasDaemon) throw new Error(L.NOT_RUNNING);
            const json = await new Promise((resolve, reject) => {
                Gio.DBus.session.call('org.xflow.Daemon', '/org/xflow/Daemon', 'org.xflow.Daemon', 'Command',
                    new GLib.Variant('(s)', [request]), new GLib.VariantType('(s)'), Gio.DBusCallFlags.NONE,
                    5000, this._cancellable, (connection, result) => {
                        try { resolve(connection.call_finish(result).deep_unpack()[0]); } catch (error) { reject(error); }
                    });
            });
            const response = L.parseResponse(json);
            if (!response) throw new Error('Invalid daemon response');
            if (!this._enabled) return;
            if (!response.ok) this._update({state: 'error', message: response.message ?? 'XFlow command failed'});
            else if (revision === this._revision && response.state) this._update({...response, mode: response.mode ?? mode});
        }).catch(error => {
            if (this._enabled) this._update({state: 'error', message: !this._hasDaemon ? L.NOT_RUNNING : 'XFlow command failed; check daemon'});
        });
    }

    _context() {
        if (Main.sessionMode.isLocked) return {app_id: null, window_id: null, selected_text: null};
        const window = global.display.focus_window;
        const app = window ? Shell.WindowTracker.get_default().get_window_app(window) : null;
        return {app_id: app?.get_id() ?? window?.get_wm_class() ?? null,
            window_id: window ? String(window.get_stable_sequence()) : null, selected_text: null};
    }

    _returnJson(invocation, result) {
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify(result)]));
    }

    _getClipboard(type) {
        return new Promise((resolve, reject) => {
            if (!this._enabled || Main.sessionMode.isLocked) { reject(new Error()); return; }
            this._clipboard.get_text(type, (_clipboard, text) => {
                if (!this._enabled || Main.sessionMode.isLocked) reject(new Error());
                else resolve(text);
            });
        });
    }

    _key(code, pressed, keyval = false) {
        this._keyboard ??= Clutter.get_default_backend().get_default_seat().create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        const state = pressed ? Clutter.KeyState.PRESSED : Clutter.KeyState.RELEASED;
        const time = GLib.get_monotonic_time();
        if (keyval) this._keyboard.notify_keyval(time, code, state);
        else this._keyboard.notify_key(time, code, state);
    }

    async _inject(text, options) {
        if (!this._enabled || Main.sessionMode.isLocked) throw new Error();
        const generation = this._injectGeneration = (this._injectGeneration ?? 0) + 1;
        this._cancelSource('_restoreSource');
        const before = options.restore_clipboard ? await this._getClipboard(St.ClipboardType.CLIPBOARD) : null;
        if (!this._enabled || Main.sessionMode.isLocked) throw new Error();
        this._clipboard.set_text(St.ClipboardType.CLIPBOARD, text);
        const fallback = {outcome: 'clipboard_only', message: 'Focus changed or is unknown; text copied'};
        if (!L.focusMatches(options.target, this._context())) return fallback;
        let outcome;
        try {
            if (options.method === 'type' && [...text].length <= L.MAX_TYPE_CHARS) {
                // Native IM commit supports Unicode without requiring its keysyms
                // in the active layout. X11/no IM focus uses short key sequences.
                if (Main.inputMethod.currentFocus && !Main.inputMethod.hasPreedit()) Main.inputMethod.commit(text);
                else {
                    // ponytail: raw typing is restricted to ASCII on the active
                    // keyboard layout; paste delivers other Unicode losslessly.
                    if (/[^\x09\x0a\x0d\x20-\x7e]/.test(text)) return fallback;
                    for (const unit of L.typeUnits(text)) {
                        const keyval = unit.keysym ?? Clutter.unicode_to_keysym(unit.codepoint);
                        this._key(keyval, true, true);
                        this._key(keyval, false, true);
                    }
                }
                outcome = 'typed';
            } else {
                const held = [];
                try {
                    for (const [code, pressed] of L.pasteKeys(options.terminal)) {
                        this._key(code, pressed, true);
                        if (pressed) held.push(code);
                        else held.splice(held.indexOf(code), 1);
                    }
                } finally { for (const code of held.reverse()) this._key(code, false, true); }
                outcome = 'pasted';
            }
        } catch { return {outcome: 'clipboard_only', message: 'Desktop delivery failed; text copied'}; }
        if (options.restore_clipboard && before !== null) {
            this._restoreSource = this._timeout(options.restore_delay_ms, () => {
                this._restoreSource = 0;
                this._getClipboard(St.ClipboardType.CLIPBOARD).then(current => {
                    if (current === text && generation === this._injectGeneration && this._enabled && !Main.sessionMode.isLocked)
                        this._clipboard.set_text(St.ClipboardType.CLIPBOARD, before);
                }).catch(() => {});
            });
        }
        return {outcome, message: null};
    }

    disable() {
        this._enabled = false;
        this._cancellable?.cancel();
        for (const action of this._grabs?.keys() ?? []) this._ungrab(action);
        for (const [object, id] of this._connections ?? []) object.disconnect(id);
        for (const id of this._sources ?? []) GLib.Source.remove(id);
        this._sources?.clear();
        if (this._subscription) Gio.DBus.session.signal_unsubscribe(this._subscription);
        if (this._daemonWatch) Gio.bus_unwatch_name(this._daemonWatch);
        if (this._busOwner) Gio.bus_unown_name(this._busOwner);
        this._shell?.unexport();
        this._pill?.remove_all_transitions();
        this._pill?.destroy();
        this._keyboard = this._shell = this._pill = this._clipboard = this._settings = this._desktop = null;
        this._escapeAction = 0;
    }
}
