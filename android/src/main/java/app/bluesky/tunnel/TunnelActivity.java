package app.bluesky.tunnel;

import android.Manifest;
import android.app.Application;
import android.app.NativeActivity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.lang.ref.WeakReference;

public final class TunnelActivity extends NativeActivity {
    static { System.loadLibrary("bluesky_obfs_t_gui"); }
    private static final int PATTERN = 41;
    private static final long MAX_PATTERN_BYTES = 16 * 1024 * 1024;
    private static Application context;
    private static WeakReference<TunnelActivity> current = new WeakReference<>(null);

    @Override public void onCreate(Bundle state) {
        context = getApplication();
        current = new WeakReference<>(this);
        super.onCreate(state);
    }

    @Override protected void onPause() {
        flushProfiles();
        super.onPause();
    }

    public static void startTunnel() {
        TunnelActivity activity = current.get();
        if (activity == null || activity.isFinishing()) {
            throw new IllegalStateException("Open the app to connect");
        }
        if (Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(
                Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            activity.runOnUiThread(() -> activity.requestPermissions(
                    new String[]{Manifest.permission.POST_NOTIFICATIONS}, 42));
        }
        context.startForegroundService(new Intent(context, TunnelService.class));
    }

    public static void stopTunnel() {
        // This instance flag prevents a delayed onDestroy from stopping a newer session.
        TunnelService service = TunnelService.current;
        if (service != null) service.stopNativeOnDestroy = false;
        context.stopService(new Intent(context, TunnelService.class));
    }

    public static void batterySettings() {
        Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                Uri.parse("package:" + context.getPackageName()));
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        context.startActivity(intent);
    }

    public static void choosePattern() {
        TunnelActivity activity = current.get();
        if (activity == null || activity.isFinishing()) {
            throw new IllegalStateException("Open the app to import a pattern");
        }
        activity.runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType("*/*");
                activity.startActivityForResult(intent, PATTERN);
            } catch (Exception error) {
                patternResult("", error.toString());
            }
        });
    }

    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PATTERN) return;
        if (result != RESULT_OK || data == null || data.getData() == null) {
            patternResult("", "");
            return;
        }
        Uri uri = data.getData();
        // Cloud document providers may block; never copy on the Android UI thread.
        new Thread(() -> {
            File copy = null;
            try {
                copy = File.createTempFile("pattern-import-", ".bin", getCacheDir());
                try (InputStream source = getContentResolver().openInputStream(uri);
                     FileOutputStream target = new FileOutputStream(copy)) {
                    if (source == null) throw new java.io.IOException("Cannot open selected file");
                    byte[] buffer = new byte[8192];
                    long size = 0;
                    int read;
                    while ((read = source.read(buffer)) != -1) {
                        size += read;
                        if (size > MAX_PATTERN_BYTES) throw new java.io.IOException("Pattern exceeds 16 MiB");
                        target.write(buffer, 0, read);
                    }
                    target.getFD().sync();
                }
                patternResult(copy.getAbsolutePath(), "");
            } catch (Exception error) {
                if (copy != null) copy.delete();
                patternResult("", error.toString());
            }
        }, "pattern-import").start();
    }

    private static native void patternResult(String path, String error);
    private static native void flushProfiles();
}
