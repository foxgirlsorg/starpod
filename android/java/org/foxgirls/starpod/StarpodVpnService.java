package org.foxgirls.starpod;

import android.app.PendingIntent;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.VpnService;
import android.os.ParcelFileDescriptor;

import java.io.IOException;

/**
 * The VPN: a tunnel that takes all of the device's traffic, IPv4 and IPv6.
 * libstarpod.so reads its packets and hands each connection to the engine's
 * local SOCKS port.
 *
 * Starpod itself (and so the engine it runs, which shares its user id) is
 * left out of the VPN, so the engine's own connections to the servers go
 * out over the real network instead of looping back in.
 */
public class StarpodVpnService extends VpnService {
    static final String ACTION_START = "org.foxgirls.starpod.VPN_START";
    static final String ACTION_STOP = "org.foxgirls.starpod.VPN_STOP";

    private ParcelFileDescriptor tunnel;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (ACTION_STOP.equals(action)) {
            close();
            stopSelf();
            return START_NOT_STICKY;
        }
        if (!ACTION_START.equals(action)) {
            // Started by the system (always-on VPN) rather than by the app:
            // the engine is not running, so there is nothing to carry.
            stopSelf();
            return START_NOT_STICKY;
        }
        close();
        try {
            Builder builder = new Builder()
                    .setSession("Starpod")
                    .setMtu(1500)
                    .addAddress("172.19.0.1", 30)
                    .addRoute("0.0.0.0", 0)
                    .addAddress("fdfe:dcba:9876::1", 126)
                    .addRoute("::", 0)
                    // Any address works: every DNS query in the tunnel is
                    // answered by libstarpod.so.
                    .addDnsServer("172.19.0.2")
                    .setConfigureIntent(PendingIntent.getActivity(this, 0,
                            new Intent(this, MainActivity.class),
                            PendingIntent.FLAG_IMMUTABLE));
            builder.addDisallowedApplication(getPackageName());
            tunnel = builder.establish();
        } catch (PackageManager.NameNotFoundException | RuntimeException e) {
            MainActivity.vpnFailed("The VPN could not start: " + e.getMessage());
            stopSelf();
            return START_NOT_STICKY;
        }
        if (tunnel == null) {
            MainActivity.vpnFailed("Starpod needs the VPN permission to connect");
            stopSelf();
            return START_NOT_STICKY;
        }
        MainActivity.vpnReady(tunnel.getFd());
        return START_NOT_STICKY;
    }

    /** The user turned the VPN off in the system settings, or another VPN took over. */
    @Override
    public void onRevoke() {
        close();
        MainActivity.vpnFailed("The VPN was turned off");
        stopSelf();
    }

    @Override
    public void onDestroy() {
        close();
        super.onDestroy();
    }

    private void close() {
        if (tunnel != null) {
            try {
                tunnel.close();
            } catch (IOException ignored) {
                // Closed already.
            }
            tunnel = null;
        }
    }
}
