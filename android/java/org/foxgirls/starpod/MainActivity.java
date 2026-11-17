package org.foxgirls.starpod;

import android.app.NativeActivity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.net.VpnService;

/**
 * The app's one activity: a NativeActivity (the UI and everything else is
 * libstarpod.so) plus the little Java the VPN needs.
 *
 * Native code calls {@link #startVpn()} once the engine runs and
 * {@link #stopVpn()} when it stops. The answer comes back through the two
 * native methods, which libstarpod.so registers when it starts.
 */
public class MainActivity extends NativeActivity {
    private static final int VPN_REQUEST = 0x5354;

    /** The VPN is up; {@code fd} is its tunnel, owned by the service. */
    static native void vpnReady(int fd);

    /** The VPN could not start, or the system closed it. */
    static native void vpnFailed(String reason);

    /** Ask for the VPN permission if needed, then start the VPN. Any thread. */
    public void startVpn() {
        runOnUiThread(() -> {
            Intent consent;
            try {
                consent = VpnService.prepare(this);
            } catch (RuntimeException e) {
                vpnFailed("Android refused to set up a VPN: " + e.getMessage());
                return;
            }
            if (consent == null) {
                command(StarpodVpnService.ACTION_START);
                return;
            }
            try {
                startActivityForResult(consent, VPN_REQUEST);
            } catch (ActivityNotFoundException e) {
                vpnFailed("This device has no VPN support");
            }
        });
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
            command(StarpodVpnService.ACTION_START);
        } else {
            vpnFailed("Starpod needs the VPN permission to connect");
        }
    }
}
