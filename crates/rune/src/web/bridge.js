// Browser text services. The textarea is a sliding UTF-16 context, never an empty key sink.
const coarse = () => matchMedia('(pointer: coarse)').matches;
const high = n => n >= 0xd800 && n <= 0xdbff;
const low = n => n >= 0xdc00 && n <= 0xdfff;
function boundary(text, at, forward) {
    if (at > 0 && at < text.length && high(text.charCodeAt(at - 1)) && low(text.charCodeAt(at))) at += forward ? 1 : -1;
    return at;
}

export function mount(canvas, callback) {
    const textarea = document.createElement('textarea');
    textarea.setAttribute('aria-label', 'Text input');
    textarea.setAttribute('autocapitalize', 'sentences');
    textarea.style.cssText = 'position:fixed;left:0;top:0;width:1px;height:1px;padding:0;border:0;opacity:0;font-size:16px;resize:none;pointer-events:none;';
    textarea.inputMode = coarse() ? 'none' : 'text';
    document.body.append(textarea);
    const h = { textarea, key: '', base: 0, value: '', composing: false, listeners: [], syncing: false };
    const send = event => {
        const message = JSON.stringify(event);
        if (h.syncing) queueMicrotask(() => callback(message));
        else callback(message);
    };
    const listen = (target, name, handler, options) => {
        target.addEventListener(name, handler, options);
        h.listeners.push(() => target.removeEventListener(name, handler, options));
    };
    function changed() {
        const next = textarea.value;
        const old = h.value;
        let start = 0;
        while (start < old.length && start < next.length && old[start] === next[start]) start++;
        start = boundary(old, start, false);
        let end = old.length, nextEnd = next.length;
        while (end > start && nextEnd > start && old[end - 1] === next[nextEnd - 1]) { end--; nextEnd--; }
        const adjusted = boundary(old, end, true) - end;
        end += adjusted; nextEnd += adjusted;
        const backward = textarea.selectionDirection === 'backward';
        // Keep composition locked through the synchronous callback: only the final sync may
        // rewrite the mirror, after both the replacement and selection have reached Rune.
        send({ kind: 'edit', key: h.key, base: h.base, expected: old,
            start: h.base + start, end: h.base + end, text: next.slice(start, nextEnd),
            anchor: h.base + (backward ? textarea.selectionEnd : textarea.selectionStart),
            focus: h.base + (backward ? textarea.selectionStart : textarea.selectionEnd) });
    }
    listen(textarea, 'input', event => { if (!h.composing && !event.isComposing) changed(); });
    listen(textarea, 'compositionstart', () => { h.composing = true; });
    listen(textarea, 'compositionupdate', event => send({ kind: 'preedit', key: h.key, text: event.data ?? '' }));
    listen(textarea, 'compositionend', () => {
        changed();
        h.composing = false;
        send({ kind: 'sync' });
    });
    listen(textarea, 'paste', event => {
        event.preventDefault();
        send({ kind: 'paste', text: event.clipboardData?.getData('text/plain') ?? '' });
    });
    listen(textarea, 'keydown', event => {
        if (event.isComposing || h.composing || event.keyCode === 229) return;
        const chord = event.metaKey || event.ctrlKey;
        // Ordinary typing, deletion and predictive replacements belong to input, not keydown.
        if (!chord && !['Enter', 'Tab', 'Escape', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'].includes(event.key)) return;
        if ((chord && event.key.toLowerCase() === 'v') || (event.key === 'Enter' && event.shiftKey)) return;
        event.preventDefault();
        send({ kind: 'key', key: event.key, shift: event.shiftKey, ctrl: event.ctrlKey,
            alt: event.altKey, meta: event.metaKey, repeat: event.repeat });
    });
    listen(textarea, 'select', () => {
        if (h.syncing || h.composing || document.activeElement !== textarea) return;
        const backward = textarea.selectionDirection === 'backward';
        send({ kind: 'selection', key: h.key, base: h.base, expected: h.value,
            anchor: h.base + (backward ? textarea.selectionEnd : textarea.selectionStart),
            focus: h.base + (backward ? textarea.selectionStart : textarea.selectionEnd) });
    });
    // winit prevents scrolling on its canvas; stop pinch zoom on the editor as well.
    listen(document, 'gesturestart', event => event.preventDefault(), { passive: false });
    return h;
}

export function sync(h, key, text, anchor, focus, activate) {
    if (h.composing && key === h.key) return;
    h.composing = false;
    const field = h.textarea;
    h.syncing = true;
    if (!key) {
        if (document.activeElement === field) field.blur();
        h.key = ''; h.value = ''; field.value = '';
        field.inputMode = coarse() ? 'none' : 'text';
        h.syncing = false;
        return;
    }
    const base = boundary(text, Math.max(0, Math.min(anchor, focus) - 512), false);
    const end = boundary(text, Math.min(text.length, Math.max(anchor, focus) + 512), true);
    const value = text.slice(base, end);
    h.syncing = true;
    h.key = key; h.base = base; h.value = value;
    if (field.value !== value) field.value = value;
    const start = Math.min(anchor, focus) - base, finish = Math.max(anchor, focus) - base;
    const direction = anchor > focus ? 'backward' : 'forward';
    if (field.selectionStart !== start || field.selectionEnd !== finish || field.selectionDirection !== direction) {
        field.setSelectionRange(start, finish, direction);
    }
    if (activate) { field.inputMode = 'text'; field.focus({ preventScroll: true }); }
    h.syncing = false;
}

export function destroy(h) { for (const remove of h.listeners) remove(); h.textarea.remove(); }
