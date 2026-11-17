package org.foxgirls.starpod;

import android.app.NativeActivity;
import android.app.PendingIntent;
import android.content.ActivityNotFoundException;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.PackageInstaller;
import android.net.VpnService;
import android.os.Build;
import android.os.Bundle;
import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * The app's one activity: a NativeActivity (the UI and everything else is
 * libstarpod.so) plus the little Java the VPN and the self-update need.
 *
 * Native code calls {@link #startVpn()} once the engine runs and
 * {@link #stopVpn()} when it stops. The answer comes back through the two
 * native methods, which libstarpod.so registers when it starts.
 */
public class MainActivity extends NativeActivity {
    private static final int VPN_REQUEST = 0x5354;
    private static final String INSTALL_STATUS = "org.foxgirls.starpod.INSTALL_STATUS";

    /** The VPN is up; {@code fd} is its tunnel, owned by the service. */
    static native void vpnReady(int fd);

    /** The VPN could not start, or the system closed it. */
    static native void vpnFailed(String reason);

    /** The update could not be installed, or Android did not finish it. */
    static native void updateFailed(String reason);

    /** Whether the user let Starpod open a VPN, the answer to {@link #requestVpnPermission()}. */
    static native void vpnPermission(boolean granted, String reason);

    /** Ask for the VPN permission if Starpod does not have it yet. Any thread. */
    public void requestVpnPermission() {
        runOnUiThread(() -> {
            Intent consent;
            try {
                consent = VpnService.prepare(this);
            } catch (RuntimeException e) {
                vpnPermission(false, "Android refused to set up a VPN: " + e.getMessage());
                return;
            }
            if (consent == null) {
                vpnPermission(true, null);
                return;
            }
            try {
                startActivityForResult(consent, VPN_REQUEST);
            } catch (ActivityNotFoundException e) {
                vpnPermission(false, "This device has no VPN support");
            }
        });
    }

    /** Start the VPN; the permission was granted. Any thread. */
    public void startVpn() {
        runOnUiThread(() -> command(StarpodVpnService.ACTION_START));
    }

    /** Stop the VPN. Any thread. */
    public void stopVpn() {
        runOnUiThread(() -> command(StarpodVpnService.ACTION_STOP));
    }

    private void command(String action) {
        try {
            startService(new Intent(this, StarpodVpnService.class).setAction(action));
        } catch (RuntimeException e) {
            if (StarpodVpnService.ACTION_START.equals(action)) {
                vpnFailed("The VPN service could not start: " + e.getMessage());
            }
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != VPN_REQUEST) {
            super.onActivityResult(requestCode, resultCode, data);
            return;
        }
        if (resultCode == RESULT_OK) {
            vpnPermission(true, null);
        } else {
            vpnPermission(false, "Starpod needs the VPN permission to connect");
        }
    }

    /** Receives the result of each update session that {@link #installApk} commits. */
    private final BroadcastReceiver installReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
            String message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
            if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                try {
                    Intent confirm;
                    if (Build.VERSION.SDK_INT >= 33) {
                        confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent.class);
                    } else {
                        confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT);
                    }
                    confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                    startActivity(confirm);
                } catch (Exception e) {
                    updateFailed("The update could not be shown to the user: " + e.getMessage());
                }
            } else if (status == PackageInstaller.STATUS_SUCCESS) {
                // Android installed the update and restarts the app; nothing to report.
            } else if (status == PackageInstaller.STATUS_FAILURE_ABORTED) {
                updateFailed("The update was cancelled");
            } else if (status == PackageInstaller.STATUS_FAILURE_CONFLICT
                    || status == PackageInstaller.STATUS_FAILURE_INCOMPATIBLE) {
                updateFailed("Android did not install the update: it is not signed like the installed Starpod");
            } else {
                updateFailed("Android did not install the update: " + (message != null ? message : "unknown error"));
            }
        }
    };

    /**
     * Where Android extracted the APK's native libraries, among them the
     * engine (libmeow.so): the one place an app may run a binary from.
     */
    public String nativeLibraryDir() {
        return getApplicationInfo().nativeLibraryDir;
    }

    /**
     * The package that installed this app, such as an app store, or null when
     * Android does not say. Any thread.
     */
    public String installerPackage() {
        try {
            if (Build.VERSION.SDK_INT >= 30) {
                return getPackageManager().getInstallSourceInfo(getPackageName()).getInstallingPackageName();
            }
            return getPackageManager().getInstallerPackageName(getPackageName());
        } catch (Exception e) {
            return null;
        }
    }

    /**
     * Install the APK at {@code path} as an update of this app, through a
     * PackageInstaller session. Returns null once the session is committed, or
     * a short English reason when it could not be handed to Android. The result
     * of the install arrives later through the install status receiver. Any
     * thread; the copy runs on the calling thread.
     */
    public String installApk(String path) {
        PackageInstaller installer = getPackageManager().getPackageInstaller();
        int sessionId = -1;
        try {
            File file = new File(path);
            PackageInstaller.SessionParams params =
                    new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
            params.setAppPackageName(getPackageName());
            sessionId = installer.createSession(params);
            PackageInstaller.Session session = installer.openSession(sessionId);
            try {
                try (InputStream in = new FileInputStream(file);
                     OutputStream out = session.openWrite("starpod.apk", 0, file.length())) {
                    byte[] buffer = new byte[65536];
                    int n;
                    while ((n = in.read(buffer)) != -1) {
                        out.write(buffer, 0, n);
                    }
                    session.fsync(out);
                }
                Intent status = new Intent(INSTALL_STATUS).setPackage(getPackageName());
                PendingIntent pending = PendingIntent.getBroadcast(this, sessionId, status,
                        PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_MUTABLE);
                session.commit(pending.getIntentSender());
            } finally {
                session.close();
            }
            return null;
        } catch (Exception e) {
            if (sessionId >= 0) {
                try {
                    installer.abandonSession(sessionId);
                } catch (RuntimeException ignored) {
                    // The session is already gone; nothing else to do.
                }
            }
            return "The update could not be handed to Android: " + e.getMessage();
        }
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        IntentFilter filter = new IntentFilter(INSTALL_STATUS);
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(installReceiver, filter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(installReceiver, filter);
        }
    }

    @Override
    protected void onDestroy() {
        unregisterReceiver(installReceiver);
        super.onDestroy();
    }
}
