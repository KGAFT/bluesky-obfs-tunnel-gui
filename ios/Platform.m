#import <UIKit/UIKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>

extern void bluesky_pattern_result(const char *path, const char *error);
extern void bluesky_background_expired(void);
extern void bluesky_flush_profiles(void);

static UIViewController *presenter(void) {
    for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
        if (scene.activationState != UISceneActivationStateForegroundActive ||
            ![scene isKindOfClass:UIWindowScene.class]) continue;
        for (UIWindow *window in ((UIWindowScene *)scene).windows) {
            if (!window.isKeyWindow) continue;
            UIViewController *controller = window.rootViewController;
            while (controller.presentedViewController) controller = controller.presentedViewController;
            return controller;
        }
    }
    return nil;
}

@interface BlueskyDocumentDelegate : NSObject <UIDocumentPickerDelegate>
@end
@implementation BlueskyDocumentDelegate
- (void)documentPickerWasCancelled:(UIDocumentPickerViewController *)controller {
    bluesky_pattern_result("", "");
}
- (void)documentPicker:(UIDocumentPickerViewController *)controller didPickDocumentsAtURLs:(NSArray<NSURL *> *)urls {
    NSURL *url = urls.firstObject;
    if (!url) { bluesky_pattern_result("", ""); return; }
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        BOOL scoped = [url startAccessingSecurityScopedResource];
        NSFileCoordinator *coordinator = [[NSFileCoordinator alloc] initWithFilePresenter:nil];
        NSError *coordinationError = nil;
        __block NSString *resultPath = @"";
        __block NSString *message = @"";
        [coordinator coordinateReadingItemAtURL:url options:0 error:&coordinationError byAccessor:^(NSURL *readURL) {
            NSError *error = nil;
            NSNumber *size = nil;
            [readURL getResourceValue:&size forKey:NSURLFileSizeKey error:&error];
            if (error || !size || size.unsignedLongLongValue == 0 || size.unsignedLongLongValue > 16 * 1024 * 1024) {
                message = error.localizedDescription ?: @"Pattern must contain 1 byte to 16 MiB.";
                return;
            }
            NSData *bytes = [NSData dataWithContentsOfURL:readURL options:NSDataReadingMappedIfSafe error:&error];
            if (!bytes || bytes.length > 16 * 1024 * 1024) {
                message = error.localizedDescription ?: @"Pattern exceeds 16 MiB.";
                return;
            }
            NSString *path = [NSTemporaryDirectory() stringByAppendingPathComponent:
                [NSString stringWithFormat:@"pattern-import-%@.bin", NSUUID.UUID.UUIDString]];
            if (![bytes writeToFile:path options:NSDataWritingAtomic error:&error]) {
                message = error.localizedDescription;
                return;
            }
            resultPath = path;
        }];
        if (scoped) [url stopAccessingSecurityScopedResource];
        if (coordinationError) message = coordinationError.localizedDescription;
        bluesky_pattern_result(resultPath.UTF8String, message.UTF8String);
    });
}
@end

void bluesky_ios_choose_pattern(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        static BlueskyDocumentDelegate *delegate;
        if (!delegate) delegate = [BlueskyDocumentDelegate new];
        UIViewController *controller = presenter();
        if (!controller) { bluesky_pattern_result("", "Cannot present Files in the current scene."); return; }
        UIDocumentPickerViewController *picker = [[UIDocumentPickerViewController alloc]
            initForOpeningContentTypes:@[UTTypeData, UTTypeItem] asCopy:YES];
        picker.allowsMultipleSelection = NO;
        picker.delegate = delegate;
        [controller presentViewController:picker animated:YES completion:nil];
    });
}

static BOOL tunnelActive;
static UIBackgroundTaskIdentifier backgroundTask = UIBackgroundTaskInvalid;
void bluesky_ios_init(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSNotificationCenter.defaultCenter addObserverForName:UIApplicationWillResignActiveNotification object:nil
            queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *notification) {
                bluesky_flush_profiles();
            }];
    });
}
static void endBackgroundTask(void) {
    if (backgroundTask == UIBackgroundTaskInvalid) return;
    [UIApplication.sharedApplication endBackgroundTask:backgroundTask];
    backgroundTask = UIBackgroundTaskInvalid;
}

void bluesky_ios_start(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        static dispatch_once_t once;
        dispatch_once(&once, ^{
            NSNotificationCenter *center = NSNotificationCenter.defaultCenter;
            [center addObserverForName:UIApplicationDidEnterBackgroundNotification object:nil
                queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *notification) {
                    if (!tunnelActive || backgroundTask != UIBackgroundTaskInvalid) return;
                    backgroundTask = [UIApplication.sharedApplication beginBackgroundTaskWithName:@"Finish tunnel session"
                        expirationHandler:^{
                            tunnelActive = NO;
                            bluesky_background_expired();
                            endBackgroundTask();
                        }];
                    if (backgroundTask == UIBackgroundTaskInvalid) {
                        tunnelActive = NO;
                        bluesky_background_expired();
                    }
                }];
            [center addObserverForName:UIApplicationWillEnterForegroundNotification object:nil
                queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *notification) {
                    endBackgroundTask();
                }];
        });
        tunnelActive = YES;
    });
}

void bluesky_ios_stop(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        tunnelActive = NO;
        endBackgroundTask();
    });
}
