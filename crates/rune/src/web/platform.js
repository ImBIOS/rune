export async function writeClipboard(text) {
    return navigator.clipboard?.writeText(text) ?? Promise.reject(new Error('Clipboard requires HTTPS.'));
}

export async function readClipboard() {
    return navigator.clipboard?.readText() ?? Promise.reject(new Error('Clipboard requires HTTPS.'));
}

export function openLink(url) {
    const parsed = new URL(url, location.href);
    if (!['https:', 'http:', 'mailto:'].includes(parsed.protocol)) throw new Error('Unsupported link scheme.');
    window.open(parsed.href, '_blank', 'noopener,noreferrer');
}

export function viewport(canvas, callback) {
    const probe = document.createElement('div');
    probe.style.cssText = 'position:fixed;visibility:hidden;pointer-events:none;padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left)';
    document.body.append(probe);
    const update = () => {
        const v = window.visualViewport;
        canvas.style.width = `${v?.width ?? innerWidth}px`;
        canvas.style.height = `${v?.height ?? innerHeight}px`;
        canvas.style.left = `${v?.offsetLeft ?? 0}px`;
        canvas.style.top = `${v?.offsetTop ?? 0}px`;
        const s = getComputedStyle(probe);
        callback(JSON.stringify({ kind: 'viewport', insets: [s.paddingTop, s.paddingRight, s.paddingBottom, s.paddingLeft].map(parseFloat) }));
    };
    const motion = matchMedia('(prefers-reduced-motion: reduce)');
    const reduced = () => callback(JSON.stringify({ kind: 'motion', reduced: motion.matches }));
    const focus = () => callback(JSON.stringify({ kind: 'focus', focused: document.hasFocus() }));
    const visibility = () => callback(JSON.stringify({ kind: 'visibility', visible: !document.hidden }));
    window.visualViewport?.addEventListener('resize', update);
    window.visualViewport?.addEventListener('scroll', update);
    window.addEventListener('resize', update);
    motion.addEventListener('change', reduced);
    window.addEventListener('focus', focus);
    window.addEventListener('blur', focus);
    document.addEventListener('visibilitychange', visibility);
    // Mount is called while Rust is installing the shell; enter it only after installation.
    queueMicrotask(() => { update(); reduced(); focus(); visibility(); });
    return () => {
        window.visualViewport?.removeEventListener('resize', update);
        window.visualViewport?.removeEventListener('scroll', update);
        window.removeEventListener('resize', update);
        motion.removeEventListener('change', reduced);
        window.removeEventListener('focus', focus);
        window.removeEventListener('blur', focus);
        document.removeEventListener('visibilitychange', visibility);
        probe.remove();
    };
}
