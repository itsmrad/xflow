// Run: node --test packaging/gnome-extension/tests/
import assert from 'node:assert/strict';
import {test} from 'node:test';
import * as L from '../logic.js';

test('parseEvent validates state, clamps level and defaults mode', () => {
    assert.deepEqual(L.parseEvent('{"state":"listening","level":0.42,"message":null,"mode":"command"}'),
        {state: 'listening', level: 0.42, message: null, mode: 'command'});
    assert.deepEqual(L.parseEvent('{"state":"listening","level":7}'),
        {state: 'listening', level: 1, message: null, mode: 'dictation'});
    assert.equal(L.parseEvent('{"state":"listening","level":"x"}').level, 0);
    assert.equal(L.parseEvent('{"state":"recording"}'), null);
    assert.equal(L.parseEvent('[1]'), null);
    assert.equal(L.parseEvent('not json'), null);
    assert.equal(L.parseEvent(42), null);
});

test('parseResponse keeps ok/state/message and rejects junk', () => {
    assert.deepEqual(L.parseResponse('{"ok":false,"state":"error","level":0,"message":"busy"}'),
        {ok: false, state: 'error', level: 0, message: 'busy', mode: null, injection: null});
    assert.equal(L.parseResponse('{"state":"idle"}'), null);
    assert.equal(L.parseResponse('{"ok":true,"state":"weird"}').state, null);
});

test('messages are single-line and truncated at code points', () => {
    assert.equal(L.truncateMessage('  a\n b\tc '), 'a b c');
    assert.equal(L.truncateMessage(''), null);
    assert.equal(L.truncateMessage(null), null);
    const long = '😀'.repeat(100);
    const cut = L.truncateMessage(long, 10);
    assert.equal([...cut].length, 10);
    assert.ok(cut.endsWith('…'));
});

test('truncateUtf8 never splits a code point', () => {
    assert.equal(L.truncateUtf8('abc', 2), 'ab');
    assert.equal(L.truncateUtf8('é', 1), '');
    assert.equal(L.truncateUtf8('a😀b', 5), 'a😀');
    assert.equal(L.utf8Length('a😀é'), 7);
});

test('commandRequest carries metadata only for start/toggle', () => {
    const context = {app_id: 'org.gnome.TextEditor.desktop', window_id: '42', selected_text: null};
    assert.deepEqual(JSON.parse(L.commandRequest('toggle', {mode: 'command', context, t0Us: 123})),
        {command: 'toggle', mode: 'command', context, t0_us: 123});
    assert.deepEqual(JSON.parse(L.commandRequest('stop', {context, t0Us: 1})), {command: 'stop'});
    assert.deepEqual(JSON.parse(L.commandRequest('start', {mode: 'bogus'})), {command: 'start', mode: 'dictation'});
    assert.throws(() => L.commandRequest('shutdown'), RangeError);
});

test('optimistic state mirrors what the daemon will do', () => {
    assert.equal(L.optimisticState('start', 'idle'), 'listening');
    assert.equal(L.optimisticState('toggle', 'idle'), 'listening');
    assert.equal(L.optimisticState('toggle', 'listening'), 'processing');
    assert.equal(L.optimisticState('stop', 'listening'), 'processing');
    assert.equal(L.optimisticState('stop', 'idle'), null);
    assert.equal(L.optimisticState('cancel', 'processing'), 'idle');
    assert.equal(L.optimisticState('copy_last', 'idle'), null);
});

test('parseInjectOptions applies defaults and rejects malformed input', () => {
    assert.deepEqual(L.parseInjectOptions('{}'),
        {method: 'paste', terminal: false, restore_clipboard: false, restore_delay_ms: 300, target: null});
    assert.deepEqual(L.parseInjectOptions(JSON.stringify({
        method: 'type', terminal: true, restore_clipboard: true, restore_delay_ms: 999999,
        target: {app_id: 'a', window_id: null},
    })), {method: 'type', terminal: true, restore_clipboard: true, restore_delay_ms: 10000,
        target: {app_id: 'a', window_id: null}});
    assert.equal(L.parseInjectOptions('').method, 'paste');
    for (const bad of ['[]', 'x', '{"method":"xdotool"}', '{"terminal":"yes"}',
        '{"restore_delay_ms":-1}', '{"restore_delay_ms":1.5}', '{"target":[]}', '{"target":{"window_id":5}}'])
        assert.throws(() => L.parseInjectOptions(bad), TypeError, bad);
});

test('focusMatches requires a provable match', () => {
    const cur = {app_id: 'org.a.desktop', window_id: '7'};
    assert.equal(L.focusMatches(null, cur), false);
    assert.equal(L.focusMatches({app_id: null, window_id: null}, cur), false);
    assert.equal(L.focusMatches({app_id: 'org.a.desktop', window_id: '7'}, cur), true);
    assert.equal(L.focusMatches({app_id: null, window_id: '7'}, cur), true);
    assert.equal(L.focusMatches({app_id: 'org.b.desktop', window_id: '7'}, cur), false);
    assert.equal(L.focusMatches({app_id: 'org.a.desktop', window_id: '8'}, cur), false);
    assert.equal(L.focusMatches({app_id: 'org.a.desktop', window_id: null}, cur), true);
    assert.equal(L.focusMatches({app_id: 'org.a.desktop', window_id: '7'}, {app_id: null, window_id: null}), false);
});

test('paste chords press and release symmetrically', () => {
    assert.deepEqual(L.pasteKeys(false), [[29, true], [47, true], [47, false], [29, false]]);
    assert.deepEqual(L.pasteKeys(true),
        [[29, true], [42, true], [47, true], [47, false], [42, false], [29, false]]);
});

test('typeUnits maps newlines and tabs to keysyms', () => {
    assert.deepEqual(L.typeUnits('a\r\nb\tc😀'), [
        {codepoint: 97}, {keysym: 0xff0d}, {codepoint: 98}, {keysym: 0xff09}, {codepoint: 99},
        {codepoint: 0x1f600},
    ]);
});

test('theme resolution follows the system only in auto', () => {
    assert.equal(L.resolveTheme('light', 'prefer-dark'), 'light');
    assert.equal(L.resolveTheme('dark', 'default'), 'dark');
    assert.equal(L.resolveTheme('auto', 'prefer-dark'), 'dark');
    assert.equal(L.resolveTheme('auto', 'default'), 'light');
    assert.equal(L.resolveTheme('auto', 'prefer-light'), 'light');
});

test('positions map to work-area alignment', () => {
    assert.deepEqual(L.alignmentFor('bottom-center'), {x: 'center', y: 'end'});
    assert.deepEqual(L.alignmentFor('top-right'), {x: 'end', y: 'start'});
    assert.deepEqual(L.alignmentFor('nonsense'), {x: 'center', y: 'end'});
});

test('pickMonitor falls back to the primary monitor', () => {
    const monitors = {primary: 1, focused: 2, pointer: 0, count: 3};
    assert.equal(L.pickMonitor('focused', monitors), 2);
    assert.equal(L.pickMonitor('pointer', monitors), 0);
    assert.equal(L.pickMonitor('primary', monitors), 1);
    assert.equal(L.pickMonitor('focused', {...monitors, focused: -1}), 1);
    assert.equal(L.pickMonitor('focused', {primary: 5, focused: 9, pointer: 0, count: 2}), 0);
});

test('geometry is compact, scales, and keeps bars inside the pill', () => {
    const g = L.pillGeometry();
    assert.equal(g.height, 40);
    assert.equal(g.width, g.waveWidth + 2 * g.padX);
    assert.ok(g.width >= 100 && g.width <= 130, `width ${g.width}`);
    assert.ok(g.barMax < g.height && g.barMin <= g.barMax);
    const big = L.pillGeometry({scale: 1.5, barCount: 21, barStyle: 'square'});
    assert.equal(big.height, 60);
    assert.equal(big.count, 21);
    assert.equal(big.barWidth, 6);
    assert.equal(L.pillGeometry({barCount: 1000}).count, 64);
    assert.equal(L.pillGeometry({barStyle: 'nope'}).barWidth, 3);
});

test('levelToAmplitude is a monotonic log curve', () => {
    assert.equal(L.levelToAmplitude(0), 0);
    assert.equal(L.levelToAmplitude(NaN), 0);
    assert.equal(L.levelToAmplitude(1), 1);
    assert.equal(L.levelToAmplitude(0.001), 0);
    const quiet = L.levelToAmplitude(0.01);
    const speech = L.levelToAmplitude(0.08);
    assert.ok(quiet > 0 && quiet < speech && speech < 1);
});

test('waveform attacks fast, decays slower, and is centre weighted', () => {
    const w = new L.Waveform(9, () => 1);
    w.setLevel(0.2);
    let h = w.step(16, 0, 'listening', false);
    assert.ok(h[4] > h[0], 'centre taller than edges');
    assert.equal(h[4], h[8 - 4]);
    w.reset();
    w.setLevel(0.2);
    const up = w.step(45, 0, 'listening', true)[4];
    const target = w.targets[4];
    assert.ok(up > 0.55 * target, 'one attack constant covers most of the rise');
    w.values.fill(0.8);
    w.targets.fill(0);
    const down = w.step(45, 0, 'listening', true)[4];
    assert.ok(down > 0.8 * Math.exp(-45 / 140) - 0.12, 'decay is slower than attack');
});

test('waveform keeps subtle life when quiet and none when static', () => {
    const w = new L.Waveform(7);
    const lively = [...w.step(16, 400, 'listening', true)];
    assert.ok(Math.max(...lively) > 0 && Math.max(...lively) < 0.15);
    w.reset();
    assert.equal(Math.max(...w.step(16, 400, 'listening', false)), 0);
});

test('processing shimmer travels and dims the other dots', () => {
    const w = new L.Waveform(9);
    const peak = t => {
        const h = w.step(16, t, 'processing', true);
        return h.indexOf(Math.max(...h));
    };
    assert.ok(peak(500) > peak(250));
    assert.ok(Math.min(...w.opacity) < 0.5 && Math.max(...w.opacity) > 0.9);
    assert.equal(Math.max(...w.step(16, 500, 'processing', false)), 0);
});

test('waveform step allocates no new buffers', () => {
    const w = new L.Waveform(5);
    const first = w.step(16, 0);
    assert.equal(w.step(16, 16), first);
});

test('toggle mode toggles on press and ignores release', () => {
    const m = new L.HotkeyMachine('toggle');
    assert.equal(m.press(0, false), 'toggle');
    assert.equal(m.release(1000), null);
    assert.equal(m.press(2000, true), 'toggle');
});

test('hold mode is push-to-talk', () => {
    const m = new L.HotkeyMachine('hold', 300);
    assert.equal(m.press(0, false), 'start');
    assert.equal(m.press(10, true), null, 'stale press while held');
    assert.equal(m.release(50), 'stop');
    assert.equal(m.release(60), null, 'duplicate release (key event + modifier watch)');
});

test('smart mode: long hold is push-to-talk, quick tap is hands-free', () => {
    const m = new L.HotkeyMachine('smart', 300);
    assert.equal(m.press(0, false), 'start');
    assert.equal(m.release(450), 'stop');

    assert.equal(m.press(1000, false), 'start');
    assert.equal(m.release(1120), null, 'tap latches hands-free');
    assert.equal(m.press(5000, true), 'stop', 'next press ends hands-free');
    assert.equal(m.release(5100), null, 'release of the stopping press is ignored');
});

test('smart mode reset drops a hold after the daemon stops', () => {
    const m = new L.HotkeyMachine('smart', 300);
    m.press(0, false);
    m.reset();
    assert.equal(m.release(900), null);
    m.configure('bogus', -5);
    assert.equal(m.mode, 'smart');
    assert.equal(m.thresholdMs, 0);
});

test('hold times are bounded per state', () => {
    assert.equal(L.holdTime('success', {successMs: 800, errorMs: 4000}), 800);
    assert.equal(L.holdTime('error', {successMs: 800, errorMs: 1e9}), 60000);
    assert.equal(L.holdTime('listening', {successMs: 800, errorMs: 4000}), 0);
});

test('demo level stays in a speech-like RMS range', () => {
    for (let t = 0; t < 3000; t += 37) {
        const level = L.demoLevel(t);
        assert.ok(level > 0 && level < 0.12);
    }
});
