package unknown.none.nx2android;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.RemoteInput;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.graphics.drawable.Icon;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Build;
import android.os.Bundle;

import org.json.JSONObject;

import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;

/**
 * One-tap wireless-debugging pairing. Android only pairs with a six-digit code shown in Settings, so this finds the pairing service on its own (no port to type) and asks for
 * the code in a notification, where it can be typed while the Settings dialog stays open. The pairing itself is done by the GUI server (`adb pair`), which also keeps the
 * connection afterwards (adb_auto.py). Needs Wi-Fi: Android advertises the pairing service on the local network.
 */
final class AdbPairing {
    private static final String CHANNEL = "nx2a_adb", ACTION = "unknown.none.nx2android.ADB_CODE", KEY = "code";
    private static final int NOTE = 4712;
    private static NsdManager nsd;
    private static NsdManager.DiscoveryListener listener;
    private static BroadcastReceiver receiver;
    private static volatile int port = 0;

    private AdbPairing() { }

    static synchronized void start(Context app) {
        stop(app);
        port = 0;
        note(app, "Pairing with wireless debugging", "Open Settings > Developer options > Wireless debugging > Pair device with pairing code, and keep that dialog open.", false);
        receiver = new BroadcastReceiver() {
            @Override public void onReceive(Context c, Intent i) {
                Bundle r = RemoteInput.getResultsFromIntent(i);
                CharSequence code = r == null ? null : r.getCharSequence(KEY);
                if (code == null || code.toString().trim().isEmpty()) return;
                final String digits = code.toString().trim();
                final int p = port;
                if (p == 0) { note(c, "Pairing not found yet", "Keep the \"Pair device with pairing code\" dialog open, then enter the code again.", true); return; }
                note(c, "Pairing...", "Using the code you entered.", false);
                new Thread(() -> pair(c.getApplicationContext(), p, digits)).start();
            }
        };
        IntentFilter f = new IntentFilter(ACTION);
        if (Build.VERSION.SDK_INT >= 33) app.registerReceiver(receiver, f, Context.RECEIVER_NOT_EXPORTED);
        else app.registerReceiver(receiver, f);
        nsd = (NsdManager) app.getSystemService(Context.NSD_SERVICE);
        listener = new NsdManager.DiscoveryListener() {
            @Override public void onStartDiscoveryFailed(String t, int e) { }
            @Override public void onStopDiscoveryFailed(String t, int e) { }
            @Override public void onDiscoveryStarted(String t) { }
            @Override public void onDiscoveryStopped(String t) { }
            @Override public void onServiceLost(NsdServiceInfo s) { }
            @Override public void onServiceFound(NsdServiceInfo s) {
                nsd.resolveService(s, new NsdManager.ResolveListener() {
                    @Override public void onResolveFailed(NsdServiceInfo i, int e) { }
                    @Override public void onServiceResolved(NsdServiceInfo i) {
                        port = i.getPort();
                        note(app, "Enter the pairing code", "Type the six digits shown in the Settings dialog.", true);
                    }
                });
            }
        };
        try { nsd.discoverServices("_adb-tls-pairing._tcp", NsdManager.PROTOCOL_DNS_SD, listener); }
        catch (Exception e) { note(app, "Pairing not available", String.valueOf(e), false); }
    }

    static synchronized void stop(Context app) {
        try { if (nsd != null && listener != null) nsd.stopServiceDiscovery(listener); } catch (Exception ignored) { }
        try { if (receiver != null) app.unregisterReceiver(receiver); } catch (Exception ignored) { }
        nsd = null; listener = null; receiver = null;
    }

    private static void pair(Context c, int p, String code) {
        try {
            JSONObject r = post("{\"op\":\"pair\",\"host\":\"127.0.0.1:" + p + "\",\"code\":" + JSONObject.quote(code) + "}");
            String out = r.optString("out", "");
            if (out.toLowerCase().contains("successfully paired")) {
                post("{\"op\":\"auto\"}");   // connect now; after this the app reconnects by itself
                note(c, "Paired", "adb will connect by itself whenever Wireless debugging is on.", false);
                stop(c);
            } else {
                note(c, "Pairing failed", out.isEmpty() ? "No answer. Check the code and try again." : out, true);
            }
        } catch (Exception e) {
            note(c, "Pairing failed", String.valueOf(e), true);
        }
    }

    private static JSONObject post(String body) throws Exception {
        HttpURLConnection h = (HttpURLConnection) new URL("http://127.0.0.1:" + ProotService.PORT + "/api/adb").openConnection();
        h.setRequestMethod("POST"); h.setDoOutput(true); h.setConnectTimeout(8000); h.setReadTimeout(60000);
        h.setRequestProperty("Cookie", "nx2a=" + ProotService.token);
        h.setRequestProperty("Content-Type", "application/json");
        try (OutputStream o = h.getOutputStream()) { o.write(body.getBytes(StandardCharsets.UTF_8)); }
        try (InputStream in = h.getResponseCode() < 400 ? h.getInputStream() : h.getErrorStream()) {
            java.io.ByteArrayOutputStream b = new java.io.ByteArrayOutputStream();
            byte[] buf = new byte[4096]; int n;
            while (in != null && (n = in.read(buf)) > 0) b.write(buf, 0, n);
            return new JSONObject(new String(b.toByteArray(), StandardCharsets.UTF_8));
        }
    }

    private static void note(Context c, String title, String text, boolean withInput) {
        NotificationManager nm = (NotificationManager) c.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null) return;
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Wireless debugging pairing", NotificationManager.IMPORTANCE_HIGH));
        Notification.Builder b = new Notification.Builder(c, CHANNEL).setSmallIcon(R.drawable.ic_stat_nx2a).setContentTitle(title).setContentText(text)
                .setStyle(new Notification.BigTextStyle().bigText(text)).setOnlyAlertOnce(true);
        if (withInput) {
            int flags = PendingIntent.FLAG_UPDATE_CURRENT | (Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0);   // the reply is written into the intent, so it must be mutable
            PendingIntent pi = PendingIntent.getBroadcast(c, 1, new Intent(ACTION).setPackage(c.getPackageName()), flags);
            RemoteInput ri = new RemoteInput.Builder(KEY).setLabel("Pairing code").build();
            b.addAction(new Notification.Action.Builder(Icon.createWithResource(c, R.drawable.ic_stat_nx2a), "Enter code", pi).addRemoteInput(ri).build());
        }
        nm.notify(NOTE, b.build());
    }
}
