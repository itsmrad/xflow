import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import vm from 'node:vm';
import * as L from '../logic.js';

// Exercise the real Shell service/command methods with isolated desktop seams.
// Actual GI startup, signals and shortcuts are checked in the GNOME smoke run.
const source = readFileSync(new URL('../extension.js', import.meta.url), 'utf8')
    .replace(/^import .*;\n/gm, '').replace('export default class', 'class');
const flush = () => new Promise(resolve => setImmediate(resolve));
function fixture() {
    const Main = {sessionMode: {isLocked: false}, inputMethod: {currentFocus: {}, hasPreedit: () => false, commit: () => {}}};
    const pendingCalls = [];
    const Gio = {DBusCallFlags: {NONE: 0}, DBus: {session: {call: (...args) => pendingCalls.push(args)}}};
    const GLib = {get_monotonic_time: () => 123000, Variant: class {constructor(_type, value) {this.value = value;}}, VariantType: class {}};
    const St = {ClipboardType: {CLIPBOARD: 1, PRIMARY: 2}};
    const Class = vm.runInNewContext(`${source}\nXFlowExtension`, {Main, Gio, GLib, St, L, Extension: class {}});
    const x = new Class();
    const target = {app_id: 'editor', window_id: '1'};
    let current = target;
    let clipboard = 'old clipboard';
    const keys = [], timers = [];
    Object.assign(x, {_enabled: true, _state: 'idle', _mode: 'dictation', _hasDaemon: true,
        _revision: 0, _queue: Promise.resolve(), _cancellable: null,
        _clipboard: {set_text: (_type, text) => {clipboard = text;}, get_text: (_type, callback) => callback(null, clipboard)},
        _context: () => current, _key: (...event) => keys.push(event),
        _cancelSource: field => {x[field] = 0;},
        _timeout: (_ms, callback) => {timers.push(callback); return timers.length;},
        _update: event => {x._state = event.state; x._mode = event.mode ?? x._mode; ++x._revision;},
    });
    const reply = (index, response) => {
        const callback = pendingCalls[index].at(-1);
        callback({call_finish: () => ({deep_unpack: () => [JSON.stringify(response)]})}, null);
    };
    return {x, Main, target, keys, timers, pendingCalls, reply,
        focus: value => {current = value;}, clipboard: () => clipboard,
        editClipboard: value => {clipboard = value;}};
}
const options = target => ({method: 'paste', terminal: false, restore_clipboard: true, restore_delay_ms: 300, target});

test('focus changed during clipboard snapshot copies text without sending keys', async () => {
    const f = fixture();
    let snapshot;
    f.x._clipboard.get_text = (_type, callback) => {snapshot = callback;};
    const result = f.x._inject('dictation', options(f.target));
    f.focus({app_id: 'editor', window_id: '2'});
    snapshot(null, 'old clipboard');
    assert.equal((await result).outcome, 'clipboard_only');
    assert.equal(f.clipboard(), 'dictation');
    assert.equal(f.keys.length, 0);
    assert.equal(f.timers.length, 0, 'copy-only recovery must remain on the clipboard');
});

test('locked and disabled services do not touch the clipboard or keyboard', async () => {
    const f = fixture(); f.Main.sessionMode.isLocked = true;
    await assert.rejects(f.x._inject('dictation', options(f.target)));
    f.Main.sessionMode.isLocked = false; f.x._enabled = false;
    await assert.rejects(f.x._inject('dictation', options(f.target)));
    assert.equal(f.clipboard(), 'old clipboard'); assert.equal(f.keys.length, 0);
});

test('paste releases held modifiers after a virtual-key failure', async () => {
    const f = fixture();
    f.x._key = (code, down) => {f.keys.push([code, down]); if (code === 0x76 && down) throw new Error('device unavailable');};
    assert.equal((await f.x._inject('dictation', options(f.target))).outcome, 'clipboard_only');
    assert.deepEqual(f.keys.at(-1), [0xffe3, false]);
    assert.equal(f.clipboard(), 'dictation'); assert.equal(f.timers.length, 0);
});

test('clipboard restoration preserves newer user content', async () => {
    const f = fixture();
    assert.equal((await f.x._inject('dictation', options(f.target))).outcome, 'pasted');
    f.editClipboard('new user content'); f.timers[0](); await flush();
    assert.equal(f.clipboard(), 'new user content');
});

test('clipboard restoration restores only its current injection generation', async () => {
    const f = fixture();
    await f.x._inject('same text', options(f.target));
    let snapshot;
    f.x._clipboard.get_text = (_type, callback) => {snapshot = callback;};
    f.timers[0]();
    const staleSnapshot = snapshot;
    const newer = f.x._inject('same text', options(f.target));
    snapshot(null, 'same text'); await newer;
    staleSnapshot(null, 'same text'); await flush();
    assert.equal(f.clipboard(), 'same text', 'old restoration must not overwrite the newer injection');
});

test('short Unicode typing uses the native input method', async () => {
    const f = fixture(); const commits = [];
    f.Main.inputMethod.commit = text => commits.push(text);
    assert.equal((await f.x._inject('café 😀', {...options(f.target), method: 'type'})).outcome, 'typed');
    assert.deepEqual(commits, ['café 😀']); assert.equal(f.keys.length, 0);
});

test('quick start/stop is serialized and stale start reply cannot reopen the pill', async () => {
    const f = fixture();
    f.x._command('start', 'command', 123); f.x._command('stop');
    assert.equal(f.x._state, 'processing'); await flush();
    assert.equal(f.pendingCalls.length, 1);
    f.reply(0, {ok: true, state: 'listening'}); await flush();
    assert.equal(f.pendingCalls.length, 2); assert.equal(f.x._state, 'processing');
    assert.deepEqual(f.pendingCalls.map(args => JSON.parse(args[4].value[0]).command), ['start', 'stop']);
    assert.equal(JSON.parse(f.pendingCalls[0][4].value[0]).t0_us, 123);
    f.reply(1, {ok: true, state: 'processing'}); await f.x._queue;
    assert.equal(f.x._mode, 'command');
});

test('a daemon event takes precedence over an older command reply', async () => {
    const f = fixture(); f.x._command('start'); await flush();
    f.x._update({state: 'success'});
    f.reply(0, {ok: true, state: 'listening'}); await f.x._queue;
    assert.equal(f.x._state, 'success');
});
