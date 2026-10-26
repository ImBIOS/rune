// Browser text services. Rust supplies a bounded UTF-16 context with a UTF-8 base offset.
const coarse = () => matchMedia('(pointer: coarse)').matches;
const high = n => n >= 0xd800 && n <= 0xdbff;
const low = n => n >= 0xdc00 && n <= 0xdfff;
function boundary(text, at, forward) {
    if (at > 0 && at < text.length && high(text.charCodeAt(at - 1)) && low(text.charCodeAt(at))) at += forward ? 1 : -1;
    return at;
}

function replacement(old, next, composition, caret) {
    let start = 0;
    while (start < old.length && start < next.length && old[start] === next[start]) start++;
    start = boundary(old, start, false);
    let end = old.length, nextEnd = next.length;
    while (end > start && nextEnd > start && old[end - 1] === next[nextEnd - 1]) { end--; nextEnd--; }
    const adjusted = boundary(old, end, true) - end;
    end += adjusted; nextEnd += adjusted;
    // Recomposition can contain unchanged letters (even the entire old word). Include
    // those in the replaced range, otherwise preedit would be appended to committed text.
    if (composition && caret >= composition.length && next.slice(caret - composition.length, caret) === composition) {
        const from = caret - composition.length;
        const to = caret + old.length - next.length;
        if (from <= to && to <= old.length) {
            if (old === next) { start = from; end = to; nextEnd = caret; }
            else { start = Math.min(start, from); end = Math.max(end, to); nextEnd = end + next.length - old.length; }
        }
    }
    return { start, end, text: next.slice(start, nextEnd) };
}

export function mount(canvas, callback) {
    const textarea = document.createElement('textarea');
    textarea.setAttribute('aria-label', 'Text input');
    textarea.setAttribute('autocapitalize', 'sentences');
    const password = document.createElement('input');
    password.type = 'password';
    password.setAttribute('aria-label', 'Password input');
    password.autocomplete = 'off';
    password.setAttribute('autocorrect', 'off');
    password.setAttribute('autocapitalize', 'off');
    password.spellcheck = false;
    for (const field of [textarea, password]) {
        field.style.cssText = 'position:fixed;left:0;top:0;width:1px;height:1px;padding:0;border:0;opacity:0;font-size:16px;resize:none;pointer-events:none;';
        field.tabIndex = -1;
        field.inputMode = coarse() ? 'none' : 'text';
        document.body.append(field);
    }
    const h = { canvas, textarea, password, field: textarea, key: '', base: 0, value: '',
        composing: false, composition: '', shift: false, listeners: [], syncing: false, disposed: false };
    const send = event => {
        const message = JSON.stringify(event);
        if (h.syncing) queueMicrotask(() => { if (!h.disposed) callback(message); });
        else if (!h.disposed) callback(message);
    };
    const listen = (target, name, handler, options) => {
        target.addEventListener(name, handler, options);
        h.listeners.push(() => target.removeEventListener(name, handler, options));
    };
    function changed(preedit = false) {
        const backward = textarea.selectionDirection === 'backward';
        send({ kind: preedit ? 'preedit' : 'edit', key: h.key, base: h.base, expected: h.value,
            ...replacement(h.value, textarea.value, preedit ? h.composition : '', textarea.selectionEnd),
            anchor: backward ? textarea.selectionEnd : textarea.selectionStart,
            focus: backward ? textarea.selectionStart : textarea.selectionEnd });
    }
    const key = (name, event = {}) => send({ kind: 'key', key: name, code: event.code ?? '', shift: !!event.shiftKey,
        ctrl: !!event.ctrlKey, alt: !!event.altKey, meta: !!event.metaKey, repeat: !!event.repeat });
    listen(textarea, 'input', event => changed(h.composing || event.isComposing));
    listen(textarea, 'compositionstart', () => { h.composing = true; h.composition = ''; });
    listen(textarea, 'compositionupdate', event => {
        h.composition = event.data ?? '';
        // compositionupdate can precede the DOM replacement; input observes it immediately,
        // and this microtask also covers engines that omit that intermediate input event.
        queueMicrotask(() => { if (!h.disposed && h.composing) changed(true); });
    });
    listen(textarea, 'compositionend', () => {
        changed();
        h.composing = false; h.composition = '';
        send({ kind: 'sync' });
    });
    listen(password, 'input', () => {
        const text = password.value;
        password.value = ''; // A transient input sink, never a mirror of the secret model.
        if (text) send({ kind: 'secure', key: h.key, text });
    });
    for (const field of [textarea, password]) {
        listen(field, 'paste', event => {
            event.preventDefault();
            const data = event.clipboardData;
            // The file must be taken during the event; Rust reads its bytes only for a hook.
            h.image = field === textarea ? [...(data?.items ?? [])].find(i => i.kind === 'file' && i.type === 'image/png')?.getAsFile() ?? undefined : undefined;
            try {
                send({ kind: 'paste', text: data?.getData('text/plain') ?? '', html: field === textarea ? data?.getData('text/html') ?? '' : '', image: !!h.image });
            } finally {
                if (h.syncing) queueMicrotask(() => { h.image = undefined; });
                else h.image = undefined;
            }
        });
        listen(field, 'beforeinput', event => {
            if (event.inputType === 'insertLineBreak' || event.inputType === 'insertParagraph') {
                event.preventDefault();
                if (!h.composing && !event.isComposing) key('Enter', { shiftKey: h.shift, code: 'Enter' });
            } else if (field === password && event.inputType.startsWith('delete')) {
                event.preventDefault();
                const forward = event.inputType.endsWith('Forward');
                key(forward ? 'Delete' : 'Backspace',
                    { altKey: event.inputType.includes('Word'), code: forward ? 'Delete' : 'Backspace' });
            }
        });
        listen(field, 'keydown', event => {
            h.shift = event.shiftKey;
            if (event.isComposing || h.composing || event.keyCode === 229) return;
            const chord = event.metaKey || event.ctrlKey;
            const editing = ['Enter', 'Tab', 'Escape', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'];
            if (field === password) editing.push('Backspace', 'Delete');
            if (!chord && !editing.includes(event.key)) return;
            if (chord && event.key.toLowerCase() === 'v') return;
            event.preventDefault();
            key(event.key, event);
        });
        listen(field, 'keyup', event => { h.shift = event.shiftKey; });
        listen(field, 'blur', () => { h.shift = false; });
    }
    listen(textarea, 'select', () => {
        if (h.syncing || h.composing || document.activeElement !== textarea) return;
        const backward = textarea.selectionDirection === 'backward';
        send({ kind: 'selection', key: h.key, base: h.base, expected: h.value,
            anchor: backward ? textarea.selectionEnd : textarea.selectionStart,
            focus: backward ? textarea.selectionStart : textarea.selectionEnd });
    });
    // Disable browser default focus without winit's forced canvas.focus(): a touch in an
    // already-focused field must not dismiss its software keyboard before Rune sees the tap.
    listen(canvas, 'pointerdown', event => event.preventDefault(), { capture: true });
    listen(canvas, 'keydown', event => event.preventDefault());
    listen(canvas, 'wheel', event => event.preventDefault(), { passive: false });
    listen(document, 'gesturestart', event => event.preventDefault(), { passive: false });
    queueMicrotask(() => {
        if (!h.disposed && document.activeElement === document.body) canvas.focus({ preventScroll: true });
    });
    return h;
}

export function sync(h, key, text, base, anchor, focus, secure) {
    if (h.composing && key === h.key) return;
    h.composing = false;
    h.syncing = true;
    const editing = document.activeElement === h.textarea || document.activeElement === h.password;
    h.key = key; h.base = base; h.value = text;
    h.password.value = '';
    h.field = secure ? h.password : h.textarea;
    if (!key) {
        h.textarea.value = '';
        if (editing) h.canvas.focus({ preventScroll: true });
        h.textarea.inputMode = h.password.inputMode = coarse() ? 'none' : 'text';
    } else {
        if (h.textarea.value !== text) h.textarea.value = text;
        if (!secure) {
            const start = Math.min(anchor, focus), end = Math.max(anchor, focus);
            const direction = anchor > focus ? 'backward' : 'forward';
            if (h.textarea.selectionStart !== start || h.textarea.selectionEnd !== end || h.textarea.selectionDirection !== direction) {
                h.textarea.setSelectionRange(start, end, direction);
            }
        }
        if (editing && document.activeElement !== h.field) activate(h);
    }
    h.syncing = false;
}

export function activate(h) {
    const field = h.key ? h.field : h.canvas;
    if (h.key) field.inputMode = 'text';
    if (document.activeElement !== field) field.focus({ preventScroll: true });
}

// The PNG the last paste carried, as a Uint8Array of at most `max` bytes, read once.
export async function pastedImage(h, max) {
    const file = h.image;
    h.image = undefined;
    return file && file.size <= max ? new Uint8Array(await file.arrayBuffer()) : undefined;
}

export function destroy(h) {
    h.disposed = true;
    h.image = undefined;
    for (const remove of h.listeners) remove();
    h.password.value = ''; h.textarea.value = '';
    h.password.remove(); h.textarea.remove();
}
