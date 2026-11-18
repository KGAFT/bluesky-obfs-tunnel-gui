package app.bluesky.tunnel;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;

public final class TunnelService extends Service {
    static { System.loadLibrary("bluesky_obfs_t_gui"); }
    private static final String CHANNEL = "tunnel";
    private static final String STOP = "app.bluesky.tunnel.STOP";
    static volatile TunnelService current;
    volatile boolean stopNativeOnDestroy = true;
    private PowerManager.WakeLock wakeLock;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private final Runnable renewWakeLock = new Runnable() {
        @Override public void run() {
            // Timeout bounds a leaked lock if lifecycle cleanup fails.
            wakeLock.acquire(10 * 60 * 1000L);
            handler.postDelayed(this, 5 * 60 * 1000L);
        }
    };

    @Override public void onCreate() {
        super.onCreate();
        current = this;
        PowerManager power = (PowerManager) getSystemService(POWER_SERVICE);
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Bluesky:Tunnel");
        wakeLock.setReferenceCounted(false);
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel(
                CHANNEL, "Active tunnel", NotificationManager.IMPORTANCE_LOW));
    }

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && STOP.equals(intent.getAction())) {
            stopNativeOnDestroy = false;
            stopNative();
            stopSelf();
            return START_NOT_STICKY;
        }
        // Never claim a restored service has a tunnel after process death. Secrets
        // that were not remembered are intentionally unavailable for auto-restart.
        if (intent == null) { stopNativeOnDestroy = false; stopSelf(); return START_NOT_STICKY; }
        try {
            PendingIntent open = PendingIntent.getActivity(this, 0,
                    new Intent(this, TunnelActivity.class), PendingIntent.FLAG_IMMUTABLE);
            PendingIntent stop = PendingIntent.getService(this, 1,
                    new Intent(this, TunnelService.class).setAction(STOP), PendingIntent.FLAG_IMMUTABLE);
            Notification notification = new Notification.Builder(this, CHANNEL)
                    .setSmallIcon(android.R.drawable.stat_sys_upload)
                    .setContentTitle("Bluesky Tunnel is running")
                    .setContentText("Local proxy / forwarding remains active in the background")
                    .setOngoing(true).setContentIntent(open)
                    .addAction(new Notification.Action.Builder(null, "Disconnect", stop).build())
                    .build();
            if (Build.VERSION.SDK_INT >= 34) {
                startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
            } else {
                startForeground(1, notification);
            }
            handler.removeCallbacks(renewWakeLock);
            renewWakeLock.run();
            serviceReady("");
            return START_NOT_STICKY;
        } catch (RuntimeException error) {
            android.util.Log.e("Bluesky", "Cannot keep tunnel in foreground", error);
            serviceReady(error.toString());
            stopNativeOnDestroy = false;
            stopSelf();
            return START_NOT_STICKY;
        }
    }

    @Override public void onDestroy() {
        handler.removeCallbacks(renewWakeLock);
        if (wakeLock != null && wakeLock.isHeld()) wakeLock.release();
        if (current == this) current = null;
        if (stopNativeOnDestroy) stopNative();
        stopForeground(STOP_FOREGROUND_REMOVE);
        super.onDestroy();
    }

    @Override public IBinder onBind(Intent intent) { return null; }
    private static native void stopNative();
    private static native void serviceReady(String error);
}
