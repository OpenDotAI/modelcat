#import <AppKit/AppKit.h>

static const NSTimeInterval confirmationInterval = 2.0;
static NSTimeInterval confirmationDeadline;
static NSTimer *dismissTimer;
static NSVisualEffectView *hint;

static void hideHint(void) {
    confirmationDeadline = 0;
    [dismissTimer invalidate];
    dismissTimer = nil;
    [hint removeFromSuperview];
    hint = nil;
}

static void showHint(NSWindow *window) {
    NSView *parent = window.contentView;
    if (!parent) return;
    NSString *message = @"再按一次 ⌘Q 退出";
    NSTextField *label = [NSTextField labelWithString:message];
    label.font = [NSFont systemFontOfSize:18 weight:NSFontWeightSemibold];
    label.alignment = NSTextAlignmentCenter;
    [label sizeToFit];
    NSSize size = NSMakeSize(label.frame.size.width + 48, label.frame.size.height + 28);
    hint = [[NSVisualEffectView alloc] initWithFrame:NSMakeRect(
        round(NSMidX(parent.bounds) - size.width / 2), round(NSMidY(parent.bounds) - size.height / 2),
        size.width, size.height)];
    hint.material = NSVisualEffectMaterialHUDWindow;
    hint.blendingMode = NSVisualEffectBlendingModeWithinWindow;
    hint.state = NSVisualEffectStateActive;
    hint.wantsLayer = YES;
    hint.layer.cornerRadius = 13;
    hint.layer.masksToBounds = YES;
    hint.autoresizingMask = NSViewMinXMargin | NSViewMaxXMargin | NSViewMinYMargin | NSViewMaxYMargin;
    [label setFrameOrigin:NSMakePoint(24, 14)];
    [hint addSubview:label];
    [parent addSubview:hint positioned:NSWindowAbove relativeTo:nil];
    NSAccessibilityPostNotificationWithUserInfo(parent, NSAccessibilityAnnouncementRequestedNotification,
        @{NSAccessibilityAnnouncementKey:message, NSAccessibilityPriorityKey:@(NSAccessibilityPriorityHigh)});
}

void modelcat_install_quit_shortcut(void (*quit)(void)) {
    [NSNotificationCenter.defaultCenter addObserverForName:NSApplicationDidResignActiveNotification
        object:NSApp queue:nil usingBlock:^(NSNotification * __unused notification) {
            hideHint();
        }];
    NSEventModifierFlags relevant = NSEventModifierFlagCommand | NSEventModifierFlagControl |
        NSEventModifierFlagOption | NSEventModifierFlagShift;
    [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown
        handler:^NSEvent *(NSEvent *event) {
            if ((event.modifierFlags & relevant) != NSEventModifierFlagCommand ||
                ![event.charactersIgnoringModifiers.lowercaseString isEqualToString:@"q"]) {
                if (confirmationDeadline) hideHint();
                return event;
            }
            if (event.isARepeat) return nil;

            BOOL confirmed = confirmationDeadline && event.timestamp <= confirmationDeadline;
            hideHint();
            if (confirmed) {
                quit();
            } else {
                confirmationDeadline = event.timestamp + confirmationInterval;
                showHint(NSApp.keyWindow);
                dismissTimer = [NSTimer scheduledTimerWithTimeInterval:confirmationInterval repeats:NO
                    block:^(NSTimer * __unused timer) { hideHint(); }];
            }
            return nil;
        }];
}


bool modelcat_focus_authorization(int pid) {
    NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    return [app activateWithOptions:NSApplicationActivateAllWindows];
}

bool modelcat_quit_authorization(int pid) {
    NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    return [app terminate];
}
