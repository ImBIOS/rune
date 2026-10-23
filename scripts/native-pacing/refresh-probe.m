// Minimal native CAMetalLayer presenter, independent of Rune and wgpu. Prints the display's
// power and refresh state, then measures positive presentedTime cadence and the screen's
// framebuffer-update cadence for one presentation variant.
//   refresh-probe <present|transaction|minduration|unsynced> <sub|backing> <drawables> <seconds>
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <Metal/Metal.h>
#import <CoreVideo/CoreVideo.h>

static NSString *mode, *host;
static int drawables;
static double seconds;
static CAMetalLayer *layer;
static id<MTLCommandQueue> queue;
static NSMutableArray<NSNumber *> *presented, *updates, *callbacks;
static double started;
static NSScreen *screen;

static void summary(NSString *name, NSArray<NSNumber *> *times) {
    NSArray<NSNumber *> *sorted = [times sortedArrayUsingSelector:@selector(compare:)];
    NSMutableDictionary<NSNumber *, NSNumber *> *ticks = [NSMutableDictionary dictionary];
    double first = 0, last = 0;
    for (NSUInteger i = 1; i < sorted.count; i++) {
        double gap = (sorted[i].doubleValue - sorted[i - 1].doubleValue) * 1000;
        NSNumber *key = @(lround(gap / (1000.0 / 120)));
        ticks[key] = @(ticks[key].intValue + 1);
    }
    if (sorted.count > 1) { first = sorted.firstObject.doubleValue; last = sorted.lastObject.doubleValue; }
    NSMutableString *histogram = [NSMutableString string];
    for (NSNumber *key in [ticks.allKeys sortedArrayUsingSelector:@selector(compare:)])
        [histogram appendFormat:@" %@t:%@", key, ticks[key]];
    printf("probe %s n=%lu hz=%.2f gaps_in_120hz_ticks=%s\n", name.UTF8String, (unsigned long)sorted.count,
        sorted.count > 1 ? (sorted.count - 1) / (last - first) : 0, histogram.UTF8String);
}

@interface Target : NSObject
@end
@implementation Target
- (void)tick:(CADisplayLink *)link {
    double now = CACurrentMediaTime();
    if (now - started > seconds) {
        [link invalidate];
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC / 2), dispatch_get_main_queue(), ^{
            printf("probe variant mode=%s host=%s drawables=%d presents_with_transaction=%d display_sync=%d opaque=%d format=%lu\n",
                mode.UTF8String, host.UTF8String, drawables, layer.presentsWithTransaction,
                layer.displaySyncEnabled, layer.opaque, (unsigned long)layer.pixelFormat);
            summary(@"callbacks", callbacks);
            summary(@"positive_presentations", presented);
            summary(@"screen_updates", updates);
            exit(0);
        });
        return;
    }
    [callbacks addObject:@(now)];
    if (@available(macOS 12.0, *)) {
        NSNumber *update = @(screen.lastDisplayUpdateTimestamp);
        if (update.doubleValue > 0 && ![updates.lastObject isEqual:update]) [updates addObject:update];
    }
    id<CAMetalDrawable> drawable = [layer nextDrawable];
    if (!drawable) return;
    BOOL measured = now - started > 1;
    [drawable addPresentedHandler:^(id<MTLDrawable> d) {
        if (measured && d.presentedTime > 0) dispatch_async(dispatch_get_main_queue(), ^{ [presented addObject:@(d.presentedTime)]; });
    }];
    MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
    pass.colorAttachments[0].texture = drawable.texture;
    pass.colorAttachments[0].loadAction = MTLLoadActionClear;
    pass.colorAttachments[0].storeAction = MTLStoreActionStore;
    double phase = fmod(now, 1.0);
    pass.colorAttachments[0].clearColor = MTLClearColorMake(0.1 * phase, 0.3 * phase, 0.6 * phase, 0.6 + 0.3 * phase);
    id<MTLCommandBuffer> buffer = [queue commandBuffer];
    [[buffer renderCommandEncoderWithDescriptor:pass] endEncoding];
    if ([mode isEqual:@"transaction"]) {
        [buffer commit];
        [buffer waitUntilScheduled];
        [drawable present];
    } else {
        if ([mode isEqual:@"minduration"]) [buffer presentDrawable:drawable afterMinimumDuration:1.0 / 120];
        else [buffer presentDrawable:drawable];
        [buffer commit];
    }
}
@end

int main(int argc, char **argv) {
    @autoreleasepool {
        mode = argc > 1 ? @(argv[1]) : @"present";
        host = argc > 2 ? @(argv[2]) : @"sub";
        drawables = argc > 3 ? atoi(argv[3]) : 3;
        seconds = argc > 4 ? atof(argv[4]) : 5;
        [NSApplication sharedApplication];
        NSApp.activationPolicy = NSApplicationActivationPolicyAccessory;
        screen = NSScreen.mainScreen;
        CGDirectDisplayID display = [screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
        CGDisplayModeRef displayMode = CGDisplayCopyDisplayMode(display);
        CVDisplayLinkRef cv;
        CVDisplayLinkCreateWithCGDisplay(display, &cv);
        CVTime nominal = CVDisplayLinkGetNominalOutputVideoRefreshPeriod(cv);
        printf("probe system low_power=%d thermal=%ld max_fps=%ld min_interval_ms=%.3f max_interval_ms=%.3f granularity_ms=%.3f mode_hz=%.2f cv_nominal_ms=%.3f builtin=%d asleep=%d\n",
            NSProcessInfo.processInfo.isLowPowerModeEnabled, (long)NSProcessInfo.processInfo.thermalState,
            (long)screen.maximumFramesPerSecond, screen.minimumRefreshInterval * 1000, screen.maximumRefreshInterval * 1000,
            screen.displayUpdateGranularity * 1000, CGDisplayModeGetRefreshRate(displayMode),
            nominal.timeScale ? 1000.0 * nominal.timeValue / nominal.timeScale : 0, CGDisplayIsBuiltin(display),
            CGDisplayIsAsleep(display));
        CGDisplayModeRelease(displayMode);
        CVDisplayLinkRelease(cv);

        NSRect frame = NSMakeRect(NSMidX(screen.frame) - 340, NSMaxY(screen.visibleFrame) - 200, 680, 160);
        NSPanel *panel = [[NSPanel alloc] initWithContentRect:frame
            styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel backing:NSBackingStoreBuffered defer:NO];
        panel.opaque = NO;
        panel.backgroundColor = NSColor.clearColor;
        panel.level = NSStatusWindowLevel;
        panel.hasShadow = NO;
        panel.ignoresMouseEvents = YES;
        NSView *view = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 680, 160)];
        layer = [CAMetalLayer layer];
        layer.device = MTLCreateSystemDefaultDevice();
        layer.pixelFormat = MTLPixelFormatBGRA8Unorm_sRGB;
        layer.opaque = NO;
        layer.maximumDrawableCount = drawables;
        layer.displaySyncEnabled = ![mode isEqual:@"unsynced"];
        layer.presentsWithTransaction = [mode isEqual:@"transaction"];
        layer.contentsScale = screen.backingScaleFactor;
        layer.drawableSize = CGSizeMake(680 * screen.backingScaleFactor, 160 * screen.backingScaleFactor);
        if ([host isEqual:@"backing"]) {
            view.layer = layer;
            view.wantsLayer = YES;
        } else {
            view.wantsLayer = YES;
            layer.frame = view.layer.bounds;
            [view.layer addSublayer:layer];
        }
        panel.contentView = view;
        [panel orderFrontRegardless];
        queue = [layer.device newCommandQueue];
        presented = [NSMutableArray array];
        updates = [NSMutableArray array];
        callbacks = [NSMutableArray array];
        started = CACurrentMediaTime();
        CADisplayLink *link = [view displayLinkWithTarget:[Target new] selector:@selector(tick:)];
        link.preferredFrameRateRange = CAFrameRateRangeMake(80, 120, 120);
        [link addToRunLoop:NSRunLoop.mainRunLoop forMode:NSRunLoopCommonModes];
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)((seconds + 5) * NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
            fprintf(stderr, "probe timeout\n");
            exit(2);
        });
        [NSApp run];
    }
}
