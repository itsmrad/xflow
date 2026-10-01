// Pure, gi-free logic for the XFlow GNOME extension: event/option parsing,
// waveform smoothing, push-to-talk/tap state machine, geometry and theme
// resolution. Imported by the Shell (GJS) and unit-tested with `node --test`.

export const STATES = Object.freeze(['idle', 'listening', 'processing', 'success', 'error']);
export const NOT_RUNNING = 'xflow daemon not running — run: xflow daemon start';
export const MAX_MESSAGE_CHARS = 72;
export const MAX_SELECTION_BYTES = 64 * 1024;
export const MAX_INJECT_BYTES = 1024 * 1024;
// Longer "type" requests are pasted: key-by-key delivery of long text is slow
// and racy, and the input-method path needs no per-character events.
export const MAX_TYPE_CHARS = 500;
export const DAEMON_COMMANDS = Object.freeze(['status', 'start', 'stop', 'toggle', 'cancel', 'copy_last', 'paste_last']);

/** Physical modifier bits, excluding Caps Lock and Num Lock. Symbolic Super
 * differs from the Mod4 bit returned by Shell.get_pointer(); snapshot the
 * actual chord at activation instead of assuming a virtual-to-physical map. */
export function heldModifierMask(modifiers) {
    return modifiers & 0xed;
}

export function clamp(value, low, high) {
    return Math.min(high, Math.max(low, value));
}

function finite(value, fallback) {
    return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

/** One trimmed line, cut at a code point boundary with an ellipsis. */
export function truncateMessage(text, max = MAX_MESSAGE_CHARS) {
    if (typeof text !== 'string') return null;
    const line = text.replace(/\s+/g, ' ').trim();
    if (!line) return null;
    const chars = [...line];
    return chars.length <= max ? line : `${chars.slice(0, max - 1).join('').trimEnd()}…`;
}

/** Cut a string to at most maxBytes of UTF-8 without splitting a code point. */
export function truncateUtf8(text, maxBytes) {
    let bytes = 0;
    let end = 0;
    for (const char of text) {
        const cp = char.codePointAt(0);
        const size = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
        if (bytes + size > maxBytes) return text.slice(0, end);
        bytes += size;
        end += char.length;
    }
    return text;
}

export function utf8Length(text) {
    let bytes = 0;
    for (const char of text) {
        const cp = char.codePointAt(0);
        bytes += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
    }
    return bytes;
}

function parseObject(json) {
    if (typeof json !== 'string') return null;
    try {
        const value = JSON.parse(json);
        return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
    } catch {
        return null;
    }
}

/** Validate a daemon Event signal payload; null when unusable. */
export function parseEvent(json) {
    const event = parseObject(json);
    if (!event || !STATES.includes(event.state)) return null;
    return {
        state: event.state,
        level: clamp(finite(event.level, 0), 0, 1),
        message: truncateMessage(event.message),
        mode: event.mode === 'command' ? 'command' : 'dictation',
    };
}

/** Validate a Command reply; null when unusable. */
export function parseResponse(json) {
    const response = parseObject(json);
    if (!response || typeof response.ok !== 'boolean') return null;
    return {
        ok: response.ok,
        state: STATES.includes(response.state) ? response.state : null,
        level: clamp(finite(response.level, 0), 0, 1),
        message: truncateMessage(response.message),
        mode: response.mode === 'command' ? 'command' : response.mode === 'dictation' ? 'dictation' : null,
        injection: typeof response.injection === 'string' ? response.injection : null,
    };
}

/** Serialize a daemon Command request. Only start/toggle carry dictation metadata. */
export function commandRequest(command, {mode = 'dictation', context = null, t0Us = null} = {}) {
    if (!DAEMON_COMMANDS.includes(command)) throw new RangeError(`unsupported command ${command}`);
    const request = {command};
    if (command === 'start' || command === 'toggle') {
        request.mode = mode === 'command' ? 'command' : 'dictation';
        if (context) request.context = context;
        if (Number.isFinite(t0Us)) request.t0_us = t0Us;
    }
    return JSON.stringify(request);
}

/** State shown the instant a command is sent, before the daemon replies. */
export function optimisticState(command, current) {
    switch (command) {
    case 'start': return 'listening';
    case 'stop': return current === 'listening' ? 'processing' : null;
    case 'toggle': return current === 'listening' ? 'processing' : 'listening';
    case 'cancel': return 'idle';
    default: return null;
    }
}

function nullableString(value, name) {
    if (value === undefined || value === null) return null;
    if (typeof value !== 'string') throw new TypeError(`${name} must be a string or null`);
    return value;
}

/** Validate Inject options; throws TypeError on malformed input (InvalidArgs). */
export function parseInjectOptions(json) {
    const options = json === '' ? {} : parseObject(json);
    if (!options) throw new TypeError('options must be a JSON object');
    const method = options.method ?? 'paste';
    if (method !== 'paste' && method !== 'type') throw new TypeError('method must be "paste" or "type"');
    for (const key of ['terminal', 'restore_clipboard']) {
        if (options[key] !== undefined && typeof options[key] !== 'boolean')
            throw new TypeError(`${key} must be a boolean`);
    }
    const delay = options.restore_delay_ms ?? 300;
    if (!Number.isInteger(delay) || delay < 0 || delay > 0xffffffff)
        throw new TypeError('restore_delay_ms must be a u32');
    let target = null;
    if (options.target !== undefined && options.target !== null) {
        if (typeof options.target !== 'object' || Array.isArray(options.target))
            throw new TypeError('target must be an object or null');
        target = {
            app_id: nullableString(options.target.app_id, 'target.app_id'),
            window_id: nullableString(options.target.window_id, 'target.window_id'),
        };
    }
    return {
        method,
        terminal: options.terminal ?? false,
        restore_clipboard: options.restore_clipboard ?? false,
        // Bounded so a hostile caller cannot park a timer for days.
        restore_delay_ms: Math.min(delay, 10000),
        target,
    };
}

/** True only when the focused window provably is the dictation target. */
export function focusMatches(target, current) {
    if (!target || !current || current.window_id === null) return false;
    if (target.window_id !== null) {
        if (target.window_id !== current.window_id) return false;
        return target.app_id === null || current.app_id === null || target.app_id === current.app_id;
    }
    return target.app_id !== null && target.app_id === current.app_id;
}

// Clutter keysyms for a native Ctrl+V / Ctrl+Shift+V chord.
const KEY_LEFTCTRL = 0xffe3;
const KEY_LEFTSHIFT = 0xffe1;
const KEY_V = 0x76;

/** Key events [keysym, pressed] for Ctrl+V or Ctrl+Shift+V. */
export function pasteKeys(terminal) {
    const chord = terminal ? [KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V] : [KEY_LEFTCTRL, KEY_V];
    return [...chord.map(code => [code, true]), ...chord.reverse().map(code => [code, false])];
}

const KEYSYM_RETURN = 0xff0d;
const KEYSYM_TAB = 0xff09;

/** Characters as code points, with newline/tab mapped to their keysyms. */
export function typeUnits(text) {
    const units = [];
    for (const char of text.replace(/\r\n?/g, '\n')) {
        if (char === '\n') units.push({keysym: KEYSYM_RETURN});
        else if (char === '\t') units.push({keysym: KEYSYM_TAB});
        else units.push({codepoint: char.codePointAt(0)});
    }
    return units;
}

export function resolveTheme(theme, colorScheme) {
    if (theme === 'dark' || theme === 'light') return theme;
    return colorScheme === 'prefer-dark' ? 'dark' : 'light';
}

const POSITIONS = {
    'bottom-center': ['center', 'end'],
    'top-center': ['center', 'start'],
    'bottom-left': ['start', 'end'],
    'bottom-right': ['end', 'end'],
    'top-left': ['start', 'start'],
    'top-right': ['end', 'start'],
};

/** Alignment of the pill inside the monitor work area. */
export function alignmentFor(position) {
    if (position === 'top') position = 'top-center';
    if (position === 'bottom') position = 'bottom-center';
    const [x, y] = POSITIONS[position] ?? POSITIONS['bottom-center'];
    return {x, y};
}

/** Keep the complete pill on its monitor, even with large offsets. */
export function pillPosition(area, width, height, position, margin, offsetX = 0, offsetY = 0) {
    const {x, y} = alignmentFor(position);
    const coord = (origin, size, length, align, offset) => {
        const inset = Math.min(Math.max(0, margin), Math.max(0, (size - length) / 2));
        const wanted = align === 'center' ? origin + (size - length) / 2
            : align === 'start' ? origin + inset : origin + size - length - inset;
        return Math.round(clamp(wanted + offset, origin, origin + Math.max(0, size - length)));
    };
    return {x: coord(area.x, area.width, width, x, offsetX),
        y: coord(area.y, area.height, height, y, offsetY)};
}

/** Monitor index for the placement setting, falling back to the primary. */
export function pickMonitor(choice, {primary, focused, pointer, count}) {
    const valid = index => Number.isInteger(index) && index >= 0 && index < count;
    const wanted = choice === 'focused' ? focused : choice === 'pointer' ? pointer : primary;
    if (valid(wanted)) return wanted;
    return valid(primary) ? primary : 0;
}

export const BAR_STYLES = Object.freeze({
    rounded: {width: 3, gap: 3, round: true},
    thin: {width: 2, gap: 3, round: true},
    square: {width: 4, gap: 2, round: false},
});

/** Logical-pixel geometry (before HiDPI scaling) for the user's size settings. */
export function pillGeometry({barCount = 13, barStyle = 'rounded', scale = 1} = {}) {
    const s = clamp(finite(scale, 1), 0.5, 3);
    const bar = BAR_STYLES[barStyle] ?? BAR_STYLES.rounded;
    const count = Math.round(clamp(finite(barCount, 13), 3, 64));
    const px = value => Math.max(1, Math.round(value * s));
    const barWidth = px(bar.width);
    const barGap = px(bar.gap);
    const height = px(40);
    const padX = px(16);
    const waveWidth = count * barWidth + (count - 1) * barGap;
    return {
        count,
        height,
        padX,
        spacing: px(8),
        barWidth,
        barGap,
        barRadius: bar.round ? barWidth / 2 : Math.min(1, barWidth / 4),
        barMin: barWidth,
        barMax: height - 2 * px(10),
        waveWidth,
        width: waveWidth + 2 * padX,
        handleWidth: px(36),
        handleHeight: px(8),
        iconSize: px(16),
        fontSize: px(13),
        maxWidth: px(420),
    };
}

/**
 * Perceptual loudness for an RMS level in 0..1: -52 dBFS → 0, -14 dBFS → 1.
 * Speech RMS sits far below full scale, so a log curve reads naturally.
 */
export function levelToAmplitude(level) {
    const rms = clamp(finite(level, 0), 0, 1);
    if (rms <= 0) return 0;
    return clamp((20 * Math.log10(rms) + 52) / 38, 0, 1);
}

/** Deterministic xorshift PRNG in [0, 1). */
export function makeRandom(seed = 0x9e3779b9) {
    let x = seed >>> 0 || 1;
    return () => {
        x ^= x << 13;
        x >>>= 0;
        x ^= x >>> 17;
        x ^= x << 5;
        x >>>= 0;
        return x / 0x100000000;
    };
}

const ATTACK_MS = 45;
const DECAY_MS = 140;
const RELAX_MS = 260;
const LIFE_AMPLITUDE = 0.09;
const LIFE_PERIOD_MS = 1700;
const SHIMMER_PERIOD_MS = 1150;

/**
 * Bar heights (0..1) for the recording pill. Each level sample sets per-bar
 * targets (centre-weighted with a little jitter so it reads as a voice, not a
 * pyramid); step() eases toward them with a fast attack and slower decay.
 * Buffers are reused so the per-frame path allocates nothing.
 */
export class Waveform {
    constructor(count, random = makeRandom()) {
        this.random = random;
        this.resize(count);
    }

    resize(count) {
        this.count = count;
        this.values = new Float64Array(count);
        this.targets = new Float64Array(count);
        this.envelope = new Float64Array(count);
        this.opacity = new Float64Array(count).fill(1);
        this.heights = new Float64Array(count);
        const centre = (count - 1) / 2;
        for (let i = 0; i < count; i++) {
            const d = centre ? Math.abs(i - centre) / centre : 0;
            this.envelope[i] = 1 - 0.72 * d * d;
        }
        this.amplitude = 0;
    }

    reset() {
        this.values.fill(0);
        this.targets.fill(0);
        this.opacity.fill(1);
        this.amplitude = 0;
    }

    setLevel(level) {
        this.amplitude = levelToAmplitude(level);
        for (let i = 0; i < this.count; i++)
            this.targets[i] = this.amplitude * this.envelope[i] * (0.62 + 0.38 * this.random());
    }

    /**
     * Advance by dtMs at absolute time nowMs. mode: 'listening' | 'processing'.
     * lively=false (animations off) snaps to targets with no ambient motion.
     */
    step(dtMs, nowMs, mode = 'listening', lively = true) {
        const dt = clamp(finite(dtMs, 0), 0, 100);
        const relax = Math.exp(-dt / RELAX_MS);
        const attack = 1 - Math.exp(-dt / ATTACK_MS);
        const decay = 1 - Math.exp(-dt / DECAY_MS);
        const processing = mode === 'processing';
        const shimmerAt = -2 + ((nowMs % SHIMMER_PERIOD_MS) / SHIMMER_PERIOD_MS) * (this.count + 3);
        for (let i = 0; i < this.count; i++) {
            if (processing) this.targets[i] = 0;
            else this.targets[i] *= relax;
            const target = this.targets[i];
            const value = this.values[i];
            this.values[i] = lively ? value + (target - value) * (target > value ? attack : decay) : target;
            let height = this.values[i];
            if (processing) {
                const glow = lively ? Math.exp(-((i - shimmerAt) ** 2) / 3.2) : 0;
                height = Math.max(height, 0.16 * glow);
                this.opacity[i] = 0.38 + 0.62 * glow;
            } else {
                this.opacity[i] = 1;
                if (lively) {
                    // Subtle travelling ripple so a quiet room still looks alive;
                    // it fades out as the voice gets louder.
                    const phase = (2 * Math.PI * nowMs) / LIFE_PERIOD_MS + i * 0.55;
                    const life = LIFE_AMPLITUDE * (0.5 + 0.5 * Math.sin(phase)) * this.envelope[i];
                    height += life * (1 - this.amplitude);
                }
            }
            this.heights[i] = clamp(height, 0, 1);
        }
        return this.heights;
    }
}

/** Speech-like synthetic RMS used by the settings preview. */
export function demoLevel(nowMs) {
    const t = nowMs / 1000;
    const syllables = Math.abs(Math.sin(t * 7.3)) * (0.55 + 0.45 * Math.sin(t * 1.9 + 1));
    return 0.004 + 0.11 * syllables * syllables;
}

/**
 * Dictation hotkey state machine.
 *  - toggle: every press toggles.
 *  - hold: press starts, release stops (push-to-talk).
 *  - smart: press starts at once; a release after `thresholdMs` stops
 *    (push-to-talk), a quicker tap leaves hands-free recording running until
 *    the next press.
 * press(nowMs, active) takes whether recording is active (including the
 * optimistic state) and returns the daemon command to send, or null.
 */
export class HotkeyMachine {
    constructor(mode = 'smart', thresholdMs = 300) {
        this.configure(mode, thresholdMs);
        this.reset();
    }

    configure(mode, thresholdMs) {
        this.mode = ['smart', 'hold', 'toggle'].includes(mode) ? mode : 'smart';
        this.thresholdMs = clamp(finite(thresholdMs, 300), 0, 10000);
    }

    reset() {
        this.holding = false;
        this.pressedAt = 0;
    }

    press(nowMs, active) {
        if (this.mode === 'toggle') return 'toggle';
        if (this.holding) return null; // stale: the previous release was never seen
        if (active) return 'stop';
        this.holding = true;
        this.pressedAt = nowMs;
        return 'start';
    }

    release(nowMs) {
        if (!this.holding) return null;
        this.holding = false;
        if (this.mode === 'hold') return 'stop';
        return nowMs - this.pressedAt >= this.thresholdMs ? 'stop' : null;
    }
}

/** How long a terminal state stays on screen before collapsing. */
export function holdTime(state, {successMs, errorMs}) {
    if (state === 'success') return clamp(finite(successMs, 900), 0, 60000);
    if (state === 'error') return clamp(finite(errorMs, 4000), 0, 60000);
    return 0;
}
