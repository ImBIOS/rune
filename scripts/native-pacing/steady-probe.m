// Same nextDrawable -> positive presentedTime endpoint as overlays/steady-state/steady-probe.m.
#import <AppKit/AppKit.h>
#import <QuartzCore/CAMetalLayer.h>
#import <Metal/Metal.h>
#import <objc/runtime.h>

static id<CAMetalDrawable> (*originalNext)(id, SEL);
static char requestKey, configKey;
static dispatch_source_t timer;

static NSWindow *ownerOf(CALayer *layer) {
    for (NSWindow *window in NSApp.windows) {
        CALayer *root = window.contentView.layer;
        for (CALayer *p = layer; p; p = p.superlayer)
            if (p == root) return window.contentView.window;
    }
    return nil;
}

static id<CAMetalDrawable> nextDrawable(CAMetalLayer *layer, SEL selector) {
    if (getenv("STEADY_MAX_DRAWABLES")) layer.maximumDrawableCount = atoi(getenv("STEADY_MAX_DRAWABLES"));
    if (getenv("STEADY_DISPLAY_SYNC")) layer.displaySyncEnabled = atoi(getenv("STEADY_DISPLAY_SYNC"));
    double entered = CACurrentMediaTime();
    id<CAMetalDrawable> drawable = originalNext(layer, selector);
    double acquired = CACurrentMediaTime();
    if (!drawable) return drawable;
    NSWindow *owner = ownerOf(layer);
    NSInteger windowID = owner.windowNumber;
    BOOL visible = owner.visible && owner.alphaValue > 0;
    NSNumber *request = objc_getAssociatedObject(owner, &requestKey);
    double requested = request.doubleValue;
    if (owner) objc_setAssociatedObject(owner, &requestKey, nil, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    if (!objc_getAssociatedObject(layer, &configKey)) {
        objc_setAssociatedObject(layer, &configKey, @YES, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        fprintf(stderr, "steady config window=%ld max_drawables=%lu transaction=%d display_sync=%d width=%.0f height=%.0f screen_hz=%ld\n",
            (long)windowID, (unsigned long)layer.maximumDrawableCount, layer.presentsWithTransaction,
            layer.displaySyncEnabled, layer.drawableSize.width, layer.drawableSize.height,
            (long)owner.screen.maximumFramesPerSecond);
    }
    [drawable addPresentedHandler:^(id<MTLDrawable> d) {
        fprintf(stderr, "steady frame window=%ld visible=%d requested=%.9f entered=%.9f acquired=%.9f presented=%.9f callback=%.9f\n",
            (long)windowID, visible, requested, entered, acquired, d.presentedTime, CACurrentMediaTime());
    }];
    return drawable;
}

__attribute__((constructor)) static void install(void) {
    Method method = class_getInstanceMethod(CAMetalLayer.class, @selector(nextDrawable));
    originalNext = (void *)method_setImplementation(method, (IMP)nextDrawable);
    if (!getenv("STEADY_RUNE_REDRAW")) return;
    dispatch_async(dispatch_get_main_queue(), ^{
        timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, dispatch_get_main_queue());
        dispatch_source_set_timer(timer, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC), NSEC_PER_SEC / 60, 0);
        dispatch_source_set_event_handler(timer, ^{
            for (NSWindow *window in NSApp.windows) {
                if (![window isKindOfClass:NSPanel.class] || !window.visible || window.alphaValue <= 0) continue;
                if (!objc_getAssociatedObject(window, &requestKey))
                    objc_setAssociatedObject(window, &requestKey, @(CACurrentMediaTime()), OBJC_ASSOCIATION_RETAIN_NONATOMIC);
                window.contentView.needsDisplay = YES;
            }
        });
        dispatch_resume(timer);
    });
}
