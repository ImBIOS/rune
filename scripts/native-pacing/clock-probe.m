// Diagnostic adapter only: keep Rune's admission/render path unchanged while swapping
// its CV callback source for an NSView CADisplayLink. Not linked into shipping Rune.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <CoreVideo/CoreVideo.h>

@interface RuneClockProbe : NSObject
@property(nonatomic) CVDisplayLinkRef cv;
@property(nonatomic) CVDisplayLinkOutputCallback callback;
@property(nonatomic) void *context;
@property(nonatomic, strong) CADisplayLink *ca;
@property(nonatomic) unsigned count;
@property(nonatomic) double first;
@property(nonatomic) double last;
@end
@implementation RuneClockProbe
- (void)tick:(CADisplayLink *)link {
    double now = CACurrentMediaTime();
    CVTimeStamp at = {0}, target = {0};
    at.hostTime = CVGetCurrentHostTime();
    target.hostTime = at.hostTime + (int64_t)((link.targetTimestamp - now) * CVGetHostClockFrequency());
    target.videoTimeScale = 1000000000;
    target.videoRefreshPeriod = (int64_t)((link.targetTimestamp - link.timestamp) * 1e9);
    target.flags = kCVTimeStampHostTimeValid | kCVTimeStampVideoRefreshPeriodValid;
    CVOptionFlags flags = 0;
    if (!self.count++) self.first = now;
    self.last = now;
    self.callback(self.cv, &at, &target, 0, &flags, self.context);
}
@end

static NSMutableDictionary<NSValue *, RuneClockProbe *> *clocks;
static CVReturn counted(CVDisplayLinkRef link, const CVTimeStamp *now, const CVTimeStamp *target,
                       CVOptionFlags in, CVOptionFlags *out, void *context) {
    RuneClockProbe *probe = (__bridge RuneClockProbe *)context;
    double at = CACurrentMediaTime();
    if (!probe.count++) probe.first = at;
    probe.last = at;
    return probe.callback(link, now, target, in, out, probe.context);
}
static CVReturn probeSet(CVDisplayLinkRef link, CVDisplayLinkOutputCallback callback, void *context) {
    if (!getenv("STEADY_CLOCK")) return CVDisplayLinkSetOutputCallback(link, callback, context);
    if (!clocks) clocks = [NSMutableDictionary new];
    RuneClockProbe *probe = [RuneClockProbe new];
    probe.cv = link; probe.callback = callback; probe.context = context;
    clocks[[NSValue valueWithPointer:link]] = probe;
    return CVDisplayLinkSetOutputCallback(link, counted, (__bridge void *)probe);
}
static CVReturn probeStart(CVDisplayLinkRef link) {
    RuneClockProbe *probe = clocks[[NSValue valueWithPointer:link]];
    if (!getenv("STEADY_CA") || !probe) return CVDisplayLinkStart(link);
    if (@available(macOS 14.0, *)) {
        NSView *view = nil;
        for (NSWindow *window in NSApp.windows)
            if ([window isKindOfClass:NSPanel.class] && window.visible) { view = window.contentView; break; }
        if (!view) return kCVReturnError;
        probe.ca = [view displayLinkWithTarget:probe selector:@selector(tick:)];
        probe.ca.preferredFrameRateRange = CAFrameRateRangeMake(80, 120, 120);
        [probe.ca addToRunLoop:NSRunLoop.mainRunLoop forMode:NSRunLoopCommonModes];
        return kCVReturnSuccess;
    }
    return kCVReturnError;
}
static CVReturn probeStop(CVDisplayLinkRef link) {
    RuneClockProbe *probe = clocks[[NSValue valueWithPointer:link]];
    CVReturn status = kCVReturnSuccess;
    if (probe.ca) { [probe.ca invalidate]; probe.ca = nil; }
    else status = CVDisplayLinkStop(link);
    if (probe.count > 1) fprintf(stderr, "steady clock source=%s callbacks=%u hz=%.3f\n",
        getenv("STEADY_CA") ? "ca" : "cv", probe.count, (probe.count - 1) / (probe.last - probe.first));
    probe.count = 0;
    return status;
}
#define INTERPOSE(new, old) __attribute__((used)) static struct { const void *replacement; const void *original; } \
    interpose_##old __attribute__((section("__DATA,__interpose"))) = { (const void *)&new, (const void *)&old }
INTERPOSE(probeSet, CVDisplayLinkSetOutputCallback);
INTERPOSE(probeStart, CVDisplayLinkStart);
INTERPOSE(probeStop, CVDisplayLinkStop);
