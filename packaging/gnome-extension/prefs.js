import Adw from 'gi://Adw';
import Gdk from 'gi://Gdk';
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk';
import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';
import * as L from './logic.js';

export default class XFlowPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        const connections = [];
        const watch = (key, callback) => connections.push(settings.connect(`changed::${key}`, callback));
        window.set_default_size(620, 720);
        window.search_enabled = true;
        const page = (title, icon) => {
            const p = new Adw.PreferencesPage({title, icon_name: icon}); window.add(p); return p;
        };
        const group = (p, title, description = '') => {
            const g = new Adw.PreferencesGroup({title, description}); p.add(g); return g;
        };
        const combo = (g, key, title, values, labels = values) => {
            const row = new Adw.ComboRow({title, model: Gtk.StringList.new(labels)});
            const sync = () => {
                let value = settings.get_string(key);
                if (key === 'position' && ['top', 'bottom'].includes(value)) value += '-center';
                row.selected = Math.max(0, values.indexOf(value));
            };
            sync(); watch(key, sync);
            row.connect('notify::selected', () => {
                if (row.selected < values.length && settings.get_string(key) !== values[row.selected])
                    settings.set_string(key, values[row.selected]);
            });
            g.add(row);
        };
        const number = (g, key, title, lower, upper, step, digits = 0) => {
            const row = new Adw.ActionRow({title});
            const spin = new Gtk.SpinButton({adjustment: new Gtk.Adjustment({lower, upper,
                step_increment: step, page_increment: step * 10}), digits, valign: Gtk.Align.CENTER});
            settings.bind(key, spin, 'value', Gio.SettingsBindFlags.DEFAULT);
            row.add_suffix(spin); row.activatable_widget = spin; g.add(row);
        };
        const toggle = (g, key, title, subtitle = '') => {
            const row = new Adw.ActionRow({title, subtitle});
            const widget = new Gtk.Switch({valign: Gtk.Align.CENTER});
            settings.bind(key, widget, 'active', Gio.SettingsBindFlags.DEFAULT);
            row.add_suffix(widget); row.activatable_widget = widget; g.add(row);
        };
        const appearance = page('Appearance', 'preferences-desktop-appearance-symbolic');
        const previewGroup = group(appearance, 'Recording pill', 'Settings apply immediately. The preview shows the listening state.');
        const preview = new Gtk.DrawingArea({height_request: 110, hexpand: true, accessible_role: Gtk.AccessibleRole.IMG});
        preview.update_property([Gtk.AccessibleProperty.LABEL], ['Preview of the XFlow recording pill']);
        preview.set_draw_func((_area, cr, width, height) => {
            const g = L.pillGeometry({scale: settings.get_double('scale'), barCount: settings.get_int('waveform-bars'),
                barStyle: settings.get_string('waveform-style')});
            const dark = L.resolveTheme(settings.get_string('theme'), Adw.StyleManager.get_default().dark ? 'prefer-dark' : 'default') === 'dark';
            const fit = Math.min(1, (width - 32) / g.width, (height - 24) / g.height);
            cr.translate((width - g.width * fit) / 2, (height - g.height * fit) / 2); cr.scale(fit, fit);
            const capsule = (x, y, w, h, r) => {
                cr.newSubPath(); cr.arc(x + w - r, y + r, r, -Math.PI / 2, 0);
                cr.arc(x + w - r, y + h - r, r, 0, Math.PI / 2);
                cr.arc(x + r, y + h - r, r, Math.PI / 2, Math.PI);
                cr.arc(x + r, y + r, r, Math.PI, Math.PI * 1.5); cr.closePath();
            };
            capsule(0, 3, g.width, g.height, g.height / 2); cr.setSourceRGBA(0, 0, 0, 0.10); cr.fill();
            capsule(0, 0, g.width, g.height, g.height / 2);
            const bg = dark ? 0.10 : 0.97;
            cr.setSourceRGBA(bg, bg, dark ? 0.11 : 0.95, settings.get_double('opacity')); cr.fillPreserve();
            cr.setSourceRGBA(dark ? 1 : 0, dark ? 1 : 0, dark ? 1 : 0, 0.08); cr.setLineWidth(1); cr.stroke();
            const waveform = new L.Waveform(g.count);
            waveform.setLevel(0.07);
            const bars = waveform.step(0, 400, 'listening', false);
            for (let i = 0; i < g.count; i++) {
                const h = g.barMin + (g.barMax - g.barMin) * bars[i];
                capsule(g.padX + i * (g.barWidth + g.barGap), (g.height - h) / 2, g.barWidth, h, g.barRadius);
                const fg = dark ? 0.98 : 0.07; cr.setSourceRGBA(fg, fg, fg, 1); cr.fill();
            }
        });
        previewGroup.add(preview);
        connections.push(settings.connect('changed', () => preview.queue_draw()));
        const styleManager = Adw.StyleManager.get_default();
        const styleSignal = styleManager.connect('notify::dark', () => preview.queue_draw());
        const colors = group(appearance, 'Style');
        combo(colors, 'theme', 'Theme', ['light', 'dark', 'auto'], ['Light', 'Dark', 'Follow system']);
        number(colors, 'scale', 'Size', 0.5, 3, 0.1, 1);
        number(colors, 'opacity', 'Opacity', 0.2, 1, 0.01, 2);
        number(colors, 'waveform-bars', 'Waveform bars', 3, 64, 1);
        combo(colors, 'waveform-style', 'Bar style', ['rounded', 'thin', 'square'], ['Rounded', 'Thin', 'Square']);
        const placement = group(appearance, 'Placement');
        combo(placement, 'position', 'Position', ['bottom-center', 'top-center', 'bottom-left', 'bottom-right', 'top-left', 'top-right'],
            ['Bottom center', 'Top center', 'Bottom left', 'Bottom right', 'Top left', 'Top right']);
        combo(placement, 'monitor', 'Monitor', ['primary', 'focused', 'pointer'], ['Primary', 'Focused window', 'Pointer at activation']);
        number(placement, 'margin', 'Margin', 0, 300, 1);
        number(placement, 'offset-x', 'Horizontal offset', -1000, 1000, 1);
        number(placement, 'offset-y', 'Vertical offset', -1000, 1000, 1);

        const shortcuts = page('Shortcuts', 'preferences-desktop-keyboard-shortcuts-symbolic');
        const bindings = group(shortcuts, 'Global shortcuts', 'Click a shortcut to capture a key combination. Escape cancels; Backspace clears it. Plain Escape cancels only while recording or processing.');
        let capture = null;
        for (const [key, title] of [['toggle-shortcut', 'Dictate'], ['command-shortcut', 'Voice command'],
            ['start-shortcut', 'Start recording'], ['stop-shortcut', 'Stop recording'], ['cancel-shortcut', 'Cancel'],
            ['copy-shortcut', 'Copy last transcript'], ['paste-shortcut', 'Paste last transcript']]) {
            const row = new Adw.ActionRow({title});
            const button = new Gtk.Button({valign: Gtk.Align.CENTER});
            const sync = () => {
                button.label = settings.get_strv(key).map(accel => {
                    const [valid, keyval, mods] = Gtk.accelerator_parse(accel);
                    return valid ? Gtk.accelerator_get_label(keyval, mods) : accel;
                }).join(', ') || 'Disabled';
            };
            sync(); watch(key, sync);
            button.connect('clicked', () => {
                if (capture) { capture.present(); return; }
                capture = new Gtk.Window({title: `Set ${title.toLowerCase()} shortcut`, transient_for: window,
                    modal: true, default_width: 380, default_height: 160});
                const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 18,
                    margin_top: 24, margin_bottom: 24, margin_start: 24, margin_end: 24});
                const prompt = new Gtk.Label({label: 'Press the new shortcut', wrap: true}); box.append(prompt);
                const clear = new Gtk.Button({label: 'Disable shortcut'}); box.append(clear);
                clear.connect('clicked', () => { settings.set_strv(key, []); capture.close(); });
                capture.set_child(box);
                const controller = new Gtk.EventControllerKey();
                controller.connect('key-pressed', (_controller, keyval, _keycode, state) => {
                    if (keyval === Gdk.KEY_Escape) { capture.close(); return true; }
                    if (keyval === Gdk.KEY_BackSpace) { settings.set_strv(key, []); capture.close(); return true; }
                    const mods = state & Gtk.accelerator_get_default_mod_mask();
                    if (!Gtk.accelerator_valid(keyval, mods)) return true;
                    const name = Gtk.accelerator_name(Gdk.keyval_to_lower(keyval), mods);
                    const conflict = ['toggle-shortcut', 'command-shortcut', 'start-shortcut', 'stop-shortcut', 'cancel-shortcut', 'copy-shortcut', 'paste-shortcut']
                        .find(other => other !== key && settings.get_strv(other).includes(name));
                    if (conflict) { prompt.label = 'That shortcut is already assigned to another XFlow action.'; return true; }
                    settings.set_strv(key, [name]); capture.close(); return true;
                });
                capture.add_controller(controller);
                capture.connect('close-request', () => { capture = null; return false; });
                capture.present();
            });
            row.add_suffix(button); row.activatable_widget = button; bindings.add(row);
        }
        const behavior = page('Behavior', 'preferences-system-symbolic');
        const recording = group(behavior, 'Recording');
        combo(recording, 'hotkey-mode', 'Dictate and command shortcuts', ['smart', 'hold', 'toggle'],
            ['Smart: tap to toggle, hold to talk', 'Hold to talk', 'Tap to toggle']);
        number(recording, 'ptt-threshold-ms', 'Smart hold threshold (ms)', 0, 10000, 10);
        const feedback = group(behavior, 'Feedback');
        combo(feedback, 'idle-style', 'When idle', ['hidden', 'minimal'], ['Hidden', 'Minimal handle']);
        toggle(feedback, 'animations', 'Animations', 'Also respects the system’s Reduce Motion setting.');
        toggle(feedback, 'show-messages', 'Show messages');
        number(feedback, 'success-hold-ms', 'Success duration (ms)', 0, 60000, 100);
        number(feedback, 'error-hold-ms', 'Error duration (ms)', 0, 60000, 100);
        window.connect('close-request', () => {
            capture?.close();
            for (const id of connections) settings.disconnect(id);
            styleManager.disconnect(styleSignal);
            return false;
        });
    }
}
