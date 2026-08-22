package io.github.agentsusagetray.companion;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Color;
import android.graphics.Insets;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.http.SslError;
import android.os.Bundle;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.window.OnBackInvokedDispatcher;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.inputmethod.InputMethodManager;
import android.webkit.CookieManager;
import android.webkit.SslErrorHandler;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import org.json.JSONArray;
import org.json.JSONException;

import java.net.URI;
import java.net.URISyntaxException;
import java.net.HttpURLConnection;
import java.net.URLEncoder;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;

public final class MainActivity extends Activity {
    private static final String PREFS = "connections";
    private static final String ENDPOINTS_KEY = "endpoint_bases";
    private static final String LAST_ENDPOINT_KEY = "last_endpoint";
    private static final int BACKGROUND = Color.rgb(18, 19, 21);
    private static final int SURFACE = Color.rgb(32, 33, 36);
    private static final int CONTROL = Color.rgb(42, 44, 48);
    private static final int BORDER = Color.rgb(67, 70, 75);
    private static final int TEXT = Color.rgb(243, 243, 243);
    private static final int PRIMARY = TEXT;
    private static final int PRIMARY_TEXT = Color.rgb(18, 19, 21);
    private static final int MUTED = Color.rgb(166, 168, 172);
    private static final int DANGER = Color.rgb(255, 181, 191);
    private static final int DANGER_FILL = Color.rgb(176, 48, 63);
    private static final int DANGER_BORDER = Color.rgb(216, 72, 88);
    private static final int HEALTH_TIMEOUT_MS = 3000;
    private static final long HEALTH_INTERVAL_MS = 15000;

    private final List<String> endpointBases = new ArrayList<>();
    private final List<EndpointParser.ParsedEndpoint> pairingQueue = new ArrayList<>();
    private final Set<String> attemptedBases = new LinkedHashSet<>();
    private final ExecutorService connectionExecutor = Executors.newSingleThreadExecutor();
    private final AtomicInteger connectionGeneration = new AtomicInteger();
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private SharedPreferences preferences;
    private FrameLayout root;
    private WebView webView;
    private ScrollView setupView;
    private EditText pairingInput;
    private TextView noticeView;
    private LinearLayout endpointList;
    private ImageButton setupBackButton;
    private EndpointParser.ParsedEndpoint pendingPairing;
    private String pairingPreferredBase;
    private boolean mainFrameFailed;
    private boolean periodicHealthEnabled;
    private boolean healthCheckInProgress;
    private String currentBase;
    private ConnectivityManager connectivityManager;
    private boolean networkCallbackRegistered;
    private final ConnectivityManager.NetworkCallback networkCallback = new ConnectivityManager.NetworkCallback() {
        @Override
        public void onAvailable(Network network) {
            mainHandler.post(() -> {
                if (currentBase == null && !endpointBases.isEmpty() && pendingPairing == null) {
                    connectToPreferredEndpoint();
                } else {
                    verifyCurrentEndpoint();
                }
            });
        }

        @Override
        public void onLost(Network network) {
            mainHandler.post(MainActivity.this::verifyCurrentEndpoint);
        }
    };
    private final Runnable periodicHealthCheck = new Runnable() {
        @Override
        public void run() {
            if (!periodicHealthEnabled) return;
            verifyCurrentEndpoint();
            mainHandler.postDelayed(this, HEALTH_INTERVAL_MS);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().setStatusBarColor(BACKGROUND);
        getWindow().setNavigationBarColor(BACKGROUND);
        preferences = getSharedPreferences(PREFS, MODE_PRIVATE);
        connectivityManager = (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
        loadEndpoints();
        buildInterface();
        configureWebView();
        if (Build.VERSION.SDK_INT >= 33) {
            getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                    OnBackInvokedDispatcher.PRIORITY_DEFAULT, this::handleBack);
        }

        String incoming = incomingPairingLink(getIntent());
        if (incoming != null) {
            pairingInput.setText(incoming);
            beginPairing(incoming);
        } else if (endpointBases.isEmpty()) {
            showSetup(null);
        } else {
            connectToPreferredEndpoint();
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        String incoming = incomingPairingLink(intent);
        if (incoming != null) {
            pairingInput.setText(incoming);
            beginPairing(incoming);
        }
    }

    @Override
    protected void onPause() {
        periodicHealthEnabled = false;
        mainHandler.removeCallbacks(periodicHealthCheck);
        CookieManager.getInstance().flush();
        super.onPause();
    }

    @Override
    protected void onStart() {
        super.onStart();
        if (connectivityManager != null && !networkCallbackRegistered) {
            try {
                connectivityManager.registerDefaultNetworkCallback(networkCallback);
                networkCallbackRegistered = true;
            } catch (RuntimeException ignored) {
                networkCallbackRegistered = false;
            }
        }
    }

    @Override
    protected void onStop() {
        if (connectivityManager != null && networkCallbackRegistered) {
            try {
                connectivityManager.unregisterNetworkCallback(networkCallback);
            } catch (RuntimeException ignored) {
                // The OS may already have removed the callback while stopping.
            }
            networkCallbackRegistered = false;
        }
        super.onStop();
    }

    @Override
    protected void onResume() {
        super.onResume();
        periodicHealthEnabled = true;
        mainHandler.removeCallbacks(periodicHealthCheck);
        mainHandler.postDelayed(periodicHealthCheck, HEALTH_INTERVAL_MS);
    }

    @Override
    protected void onDestroy() {
        periodicHealthEnabled = false;
        mainHandler.removeCallbacksAndMessages(null);
        connectionGeneration.incrementAndGet();
        connectionExecutor.shutdownNow();
        if (webView != null) {
            webView.stopLoading();
            webView.setWebViewClient(null);
            webView.destroy();
        }
        super.onDestroy();
    }

    @Override
    @SuppressLint("GestureBackNavigation")
    public void onBackPressed() {
        handleBack();
    }

    private void handleBack() {
        if (setupView.getVisibility() == View.VISIBLE) {
            if (endpointBases.isEmpty()) {
                finish();
            } else {
                connectToPreferredEndpoint();
            }
            return;
        }
        showSetup(null);
    }

    private void buildInterface() {
        root = new FrameLayout(this);
        root.setBackgroundColor(BACKGROUND);
        setContentView(root);
        if (Build.VERSION.SDK_INT >= 35) {
            root.setOnApplyWindowInsetsListener((view, windowInsets) -> {
                Insets bars = windowInsets.getInsets(WindowInsets.Type.systemBars());
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom);
                return WindowInsets.CONSUMED;
            });
            root.requestApplyInsets();
        }

        webView = new WebView(this);
        webView.setBackgroundColor(BACKGROUND);
        root.addView(webView, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        LinearLayout shell = new LinearLayout(this);
        shell.setOrientation(LinearLayout.VERTICAL);
        shell.setBackground(roundedBackground(SURFACE, BORDER, 12));
        shell.setClipToOutline(true);

        LinearLayout header = new LinearLayout(this);
        header.setGravity(Gravity.CENTER_VERTICAL);
        header.setPadding(dp(12), 0, dp(12), 0);
        shell.addView(header, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(56)));

        setupBackButton = new ImageButton(this);
        setupBackButton.setImageResource(R.drawable.ic_arrow_back);
        setupBackButton.setContentDescription("Back to usage");
        setupBackButton.setPadding(dp(9), dp(9), dp(9), dp(9));
        setupBackButton.setBackgroundColor(Color.TRANSPARENT);
        setupBackButton.setOnClickListener(view -> connectToPreferredEndpoint());
        LinearLayout.LayoutParams backParams = new LinearLayout.LayoutParams(dp(40), dp(40));
        backParams.setMargins(dp(-7), 0, dp(3), 0);
        header.addView(setupBackButton, backParams);

        ImageView icon = new ImageView(this);
        icon.setImageResource(R.drawable.ic_agents_usage_mark);
        icon.setContentDescription(null);
        header.addView(icon, new LinearLayout.LayoutParams(dp(22), dp(22)));

        TextView appTitle = text("Agents Usage", 15, TEXT);
        appTitle.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        LinearLayout.LayoutParams appTitleParams = new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 1);
        appTitleParams.setMargins(dp(9), 0, 0, 0);
        header.addView(appTitle, appTitleParams);

        View divider = new View(this);
        divider.setBackgroundColor(BORDER);
        shell.addView(divider, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(1)));

        LinearLayout content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        content.setPadding(dp(16), dp(22), dp(16), dp(24));
        shell.addView(content, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        TextView title = text("Connect to your desktop", 23, TEXT);
        title.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        content.addView(title);

        TextView explanation = text(
                "Pair once, then this app automatically finds the working LAN or Tailscale route whenever you open it.",
                14, MUTED);
        explanation.setLineSpacing(dp(3), 1.0f);
        explanation.setPadding(0, dp(10), 0, dp(22));
        content.addView(explanation);

        content.addView(sectionLabel("1  ON YOUR DESKTOP"));
        TextView desktopHelp = text(
                "Open Settings → Phone companion, turn it on, then choose Show pairing QR.",
                13, MUTED);
        desktopHelp.setLineSpacing(dp(2), 1.0f);
        desktopHelp.setPadding(0, dp(7), 0, dp(18));
        content.addView(desktopHelp);

        content.addView(sectionLabel("2  ON THIS PHONE"));
        TextView phoneHelp = text(
                "Scan the QR code with your camera. Agents Usage opens and pairs automatically. "
                        + "If you copied the link, use Paste & pair below.",
                13, MUTED);
        phoneHelp.setLineSpacing(dp(2), 1.0f);
        phoneHelp.setPadding(0, dp(7), 0, dp(12));
        content.addView(phoneHelp);

        pairingInput = new EditText(this);
        pairingInput.setHint("https://desktop.example.ts.net/agents-usage/pair?token=…");
        pairingInput.setHintTextColor(Color.rgb(128, 132, 138));
        pairingInput.setTextColor(TEXT);
        pairingInput.setTextSize(14);
        pairingInput.setSingleLine(false);
        pairingInput.setMinLines(2);
        pairingInput.setMaxLines(4);
        pairingInput.setGravity(Gravity.TOP | Gravity.START);
        pairingInput.setPadding(dp(14), dp(12), dp(14), dp(12));
        pairingInput.setBackground(roundedBackground(Color.rgb(25, 26, 29), BORDER, 8));
        content.addView(pairingInput, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        LinearLayout actions = new LinearLayout(this);
        actions.setOrientation(LinearLayout.HORIZONTAL);
        actions.setGravity(Gravity.END);
        actions.setPadding(0, dp(10), 0, 0);
        Button paste = button("Paste & pair", CONTROL);
        paste.setOnClickListener(view -> pasteAndPair());
        actions.addView(paste);
        Button connect = button("Pair", PRIMARY);
        LinearLayout.LayoutParams connectParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        connectParams.setMargins(dp(8), 0, 0, 0);
        actions.addView(connect, connectParams);
        connect.setOnClickListener(view -> beginPairing(pairingInput.getText().toString()));
        content.addView(actions);

        noticeView = text("", 13, DANGER);
        noticeView.setPadding(dp(10), dp(9), dp(10), dp(9));
        noticeView.setVisibility(View.GONE);
        content.addView(noticeView);

        TextView savedTitle = text("SAVED DESKTOPS", 12, MUTED);
        savedTitle.setLetterSpacing(0.12f);
        savedTitle.setPadding(0, dp(30), 0, dp(9));
        content.addView(savedTitle);

        endpointList = new LinearLayout(this);
        endpointList.setOrientation(LinearLayout.VERTICAL);
        content.addView(endpointList);

        TextView help = text(
                "Pairing links expire after 10 minutes. Saved desktops stay paired until you remove them. "
                        + "The app tests the last working route first and switches automatically when needed.",
                13, MUTED);
        help.setLineSpacing(dp(2), 1.0f);
        help.setPadding(0, dp(24), 0, 0);
        content.addView(help);

        Button updates = button("Check for app updates", CONTROL);
        LinearLayout.LayoutParams updateParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        updateParams.setMargins(0, dp(16), 0, 0);
        content.addView(updates, updateParams);
        updates.setOnClickListener(view -> startActivity(new Intent(
                Intent.ACTION_VIEW, Uri.parse("https://github.com/rayan6ms/agents-usage/releases/latest"))));

        TextView version = text("Agents Usage " + BuildConfig.VERSION_NAME, 12, MUTED);
        version.setGravity(Gravity.CENTER_HORIZONTAL);
        version.setPadding(0, dp(9), 0, 0);
        content.addView(version);

        setupView = new ScrollView(this);
        setupView.setFillViewport(true);
        setupView.setBackgroundColor(BACKGROUND);
        setupView.setClipToPadding(false);
        setupView.setPadding(dp(10), dp(10), dp(10), dp(10));
        setupView.addView(shell, new ScrollView.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        root.addView(setupView, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        renderEndpoints();
    }

    @SuppressLint("SetJavaScriptEnabled")
    private void configureWebView() {
        WebSettings settings = webView.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setAllowFileAccess(false);
        settings.setAllowContentAccess(false);
        settings.setDatabaseEnabled(false);
        settings.setGeolocationEnabled(false);
        settings.setMediaPlaybackRequiresUserGesture(true);
        settings.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        settings.setSafeBrowsingEnabled(true);
        settings.setUserAgentString(settings.getUserAgentString() + " AgentsUsageAndroid/" + BuildConfig.VERSION_NAME);

        CookieManager cookies = CookieManager.getInstance();
        cookies.setAcceptCookie(true);
        cookies.setAcceptThirdPartyCookies(webView, false);
        WebView.setWebContentsDebuggingEnabled(BuildConfig.DEBUG);
        webView.setWebViewClient(new CompanionWebViewClient());
    }

    private void beginPairing(String link) {
        try {
            pairingQueue.clear();
            pairingQueue.addAll(EndpointParser.parseAll(link));
            pendingPairing = pairingQueue.remove(0);
            pairingPreferredBase = pendingPairing.baseUrl;
        } catch (IllegalArgumentException error) {
            showSetup(error.getMessage());
            return;
        }
        hideKeyboard();
        attemptedBases.clear();
        mainFrameFailed = false;
        showNotice("Connecting to the desktop…", false);
        setupView.setVisibility(View.VISIBLE);
        webView.setVisibility(View.INVISIBLE);
        webView.loadUrl(pairingRequestUrl(pendingPairing.pairingUrl));
    }

    private void connectToPreferredEndpoint() {
        if (endpointBases.isEmpty()) {
            showSetup(null);
            return;
        }
        String preferred = preferences.getString(LAST_ENDPOINT_KEY, endpointBases.get(0));
        pendingPairing = null;
        showNotice("Connecting to a saved desktop…", false);
        if (!endpointBases.contains(preferred)) preferred = endpointBases.get(0);
        List<String> candidates = new ArrayList<>();
        candidates.add(preferred);
        for (String base : endpointBases) {
            if (!base.equals(preferred)) candidates.add(base);
        }
        probeAndLoad(candidates, "None of the saved desktops could be reached.");
    }

    private void loadEndpoint(String baseUrl) {
        currentBase = baseUrl;
        attemptedBases.add(baseUrl);
        mainFrameFailed = false;
        setupView.setVisibility(View.GONE);
        webView.setVisibility(View.VISIBLE);
        webView.loadUrl(baseUrl);
    }

    private void tryNextEndpoint(String reason) {
        List<String> candidates = new ArrayList<>();
        for (String base : endpointBases) {
            if (!attemptedBases.contains(base)) {
                candidates.add(base);
            }
        }
        if (candidates.isEmpty()) {
            showSetup(reason == null ? "None of the saved desktops could be reached." : reason);
        } else {
            probeAndLoad(candidates, reason);
        }
    }

    private void probeAndLoad(List<String> candidates, String failureMessage) {
        int generation = connectionGeneration.incrementAndGet();
        healthCheckInProgress = true;
        connectionExecutor.execute(() -> {
            String selected = null;
            boolean unauthorized = false;
            for (String base : candidates) {
                int status = healthStatus(base);
                if (status == HttpURLConnection.HTTP_NO_CONTENT) {
                    selected = base;
                    break;
                }
                if (status == HttpURLConnection.HTTP_UNAUTHORIZED) unauthorized = true;
            }
            String healthyBase = selected;
            boolean pairingRequired = unauthorized;
            mainHandler.post(() -> {
                if (generation != connectionGeneration.get() || isFinishing() || isDestroyed()) return;
                healthCheckInProgress = false;
                if (healthyBase != null) {
                    attemptedBases.clear();
                    loadEndpoint(healthyBase);
                } else {
                    String message = pairingRequired
                            ? "A saved desktop rejected this phone. Generate a new pairing link."
                            : failureMessage;
                    showSetup(message == null ? "None of the saved desktops could be reached." : message);
                }
            });
        });
    }

    private int healthStatus(String base) {
        String cookie = CookieManager.getInstance().getCookie(base);
        return EndpointHealthProbe.check(base, cookie, HEALTH_TIMEOUT_MS);
    }

    private void verifyCurrentEndpoint() {
        if (healthCheckInProgress || currentBase == null || pendingPairing != null
                || setupView.getVisibility() == View.VISIBLE) return;
        String checkedBase = currentBase;
        healthCheckInProgress = true;
        connectionExecutor.execute(() -> {
            int status = healthStatus(checkedBase);
            mainHandler.post(() -> {
                healthCheckInProgress = false;
                if (isFinishing() || isDestroyed() || !checkedBase.equals(currentBase)) return;
                if (status != HttpURLConnection.HTTP_NO_CONTENT) {
                    List<String> candidates = new ArrayList<>();
                    for (String base : endpointBases) {
                        if (!base.equals(checkedBase)) candidates.add(base);
                    }
                    candidates.add(checkedBase);
                    probeAndLoad(candidates, "None of the saved desktops could be reached.");
                }
            });
        });
    }

    private void pairingSucceeded() {
        String baseUrl = pendingPairing.baseUrl;
        if (!endpointBases.contains(baseUrl)) endpointBases.add(baseUrl);
        saveEndpoints();
        CookieManager.getInstance().flush();
        webView.clearHistory();
        if (!pairingQueue.isEmpty()) {
            pendingPairing = pairingQueue.remove(0);
            mainFrameFailed = false;
            webView.loadUrl(pairingRequestUrl(pendingPairing.pairingUrl));
            return;
        }
        pendingPairing = null;
        pairingInput.setText("");
        Toast.makeText(this, endpointBases.size() > 1 ? "LAN and Tailscale paired" : "Desktop paired", Toast.LENGTH_SHORT).show();
        String preferred = pairingPreferredBase;
        pairingPreferredBase = null;
        if (preferred != null && endpointBases.contains(preferred)) {
            preferences.edit().putString(LAST_ENDPOINT_KEY, preferred).apply();
        }
        if (endpointBases.size() > 1) {
            connectToPreferredEndpoint();
        } else {
            currentBase = baseUrl;
            attemptedBases.clear();
            setupView.setVisibility(View.GONE);
            webView.setVisibility(View.VISIBLE);
        }
    }

    private void showSetup(String message) {
        pendingPairing = null;
        pairingPreferredBase = null;
        pairingQueue.clear();
        webView.stopLoading();
        webView.setVisibility(View.GONE);
        setupView.setVisibility(View.VISIBLE);
        currentBase = null;
        connectionGeneration.incrementAndGet();
        healthCheckInProgress = false;
        showNotice(message, true);
        renderEndpoints();
    }

    private void showNotice(String message, boolean error) {
        if (message == null || message.trim().isEmpty()) {
            noticeView.setVisibility(View.GONE);
            return;
        }
        noticeView.setText(message);
        noticeView.setTextColor(error ? DANGER : TEXT);
        noticeView.setBackground(roundedBackground(
                error ? Color.rgb(50, 32, 37) : CONTROL,
                error ? Color.rgb(113, 64, 74) : BORDER,
                7));
        noticeView.setVisibility(View.VISIBLE);
    }

    private void renderEndpoints() {
        endpointList.removeAllViews();
        setupBackButton.setVisibility(endpointBases.isEmpty() ? View.GONE : View.VISIBLE);
        if (endpointBases.isEmpty()) {
            TextView empty = text("No desktops paired yet.", 14, MUTED);
            empty.setPadding(0, dp(8), 0, dp(8));
            endpointList.addView(empty);
            return;
        }
        for (String base : new ArrayList<>(endpointBases)) {
            LinearLayout row = new LinearLayout(this);
            row.setOrientation(LinearLayout.VERTICAL);
            row.setPadding(dp(12), dp(11), dp(12), dp(11));
            row.setBackground(roundedBackground(CONTROL, BORDER, 8));

            LinearLayout labels = new LinearLayout(this);
            labels.setOrientation(LinearLayout.VERTICAL);
            TextView host = text(displayHost(base), 15, TEXT);
            TextView url = text(base, 12, MUTED);
            labels.addView(host);
            labels.addView(url);
            row.addView(labels, new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

            LinearLayout endpointActions = new LinearLayout(this);
            endpointActions.setOrientation(LinearLayout.HORIZONTAL);
            endpointActions.setGravity(Gravity.END);
            endpointActions.setPadding(0, dp(10), 0, 0);

            Button use = button("Connect", PRIMARY);
            use.setOnClickListener(view -> {
                attemptedBases.clear();
                pendingPairing = null;
                List<String> candidate = new ArrayList<>();
                candidate.add(base);
                probeAndLoad(candidate, "That desktop could not be reached.");
            });
            endpointActions.addView(use);
            Button remove = dangerButton("Remove");
            remove.setOnClickListener(view -> removeEndpoint(base));
            LinearLayout.LayoutParams removeParams = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            removeParams.setMargins(dp(8), 0, 0, 0);
            endpointActions.addView(remove, removeParams);
            row.addView(endpointActions);

            LinearLayout.LayoutParams rowParams = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            rowParams.setMargins(0, 0, 0, dp(8));
            endpointList.addView(row, rowParams);
        }
    }

    private void removeEndpoint(String base) {
        CookieManager.getInstance().setCookie(base, "agents_usage_mobile=; Path=" + cookiePath(base)
                + "; Max-Age=0; HttpOnly; SameSite=Strict");
        CookieManager.getInstance().flush();
        endpointBases.remove(base);
        if (base.equals(preferences.getString(LAST_ENDPOINT_KEY, null))) {
            preferences.edit().remove(LAST_ENDPOINT_KEY).apply();
        }
        saveEndpoints();
        renderEndpoints();
    }

    private void loadEndpoints() {
        endpointBases.clear();
        String raw = preferences.getString(ENDPOINTS_KEY, "[]");
        try {
            JSONArray array = new JSONArray(raw);
            for (int index = 0; index < array.length(); index++) {
                String base = array.optString(index, "");
                if ((base.startsWith("https://") || base.startsWith("http://")) && !endpointBases.contains(base)) {
                    endpointBases.add(base);
                }
            }
        } catch (JSONException ignored) {
            preferences.edit().remove(ENDPOINTS_KEY).apply();
        }
    }

    private void saveEndpoints() {
        JSONArray array = new JSONArray();
        for (String base : endpointBases) array.put(base);
        preferences.edit().putString(ENDPOINTS_KEY, array.toString()).apply();
    }

    private String incomingPairingLink(Intent intent) {
        if (intent == null) return null;
        Uri data = intent.getData();
        if (data != null && "agents-usage".equalsIgnoreCase(data.getScheme())) return data.toString();
        if (Intent.ACTION_SEND.equals(intent.getAction()) && "text/plain".equals(intent.getType())) {
            return intent.getStringExtra(Intent.EXTRA_TEXT);
        }
        return null;
    }

    private void pasteAndPair() {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (clipboard == null || !clipboard.hasPrimaryClip()) {
            showSetup("The clipboard is empty.");
            return;
        }
        ClipData clip = clipboard.getPrimaryClip();
        CharSequence value = clip == null || clip.getItemCount() == 0 ? null : clip.getItemAt(0).coerceToText(this);
        if (value == null) {
            showSetup("The clipboard does not contain text.");
            return;
        }
        pairingInput.setText(value.toString().trim());
        pairingInput.setSelection(pairingInput.length());
        noticeView.setVisibility(View.GONE);
        beginPairing(pairingInput.getText().toString());
    }

    private void hideKeyboard() {
        InputMethodManager keyboard = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        View focus = getCurrentFocus();
        if (keyboard != null && focus != null) keyboard.hideSoftInputFromWindow(focus.getWindowToken(), 0);
        pairingInput.clearFocus();
    }

    private boolean isAllowed(String url) {
        if ("about:blank".equals(url)) return true;
        if (pendingPairing != null && EndpointParser.belongsToBase(url, pendingPairing.baseUrl)) return true;
        for (String base : endpointBases) {
            if (EndpointParser.belongsToBase(url, base)) return true;
        }
        return false;
    }

    private String displayHost(String base) {
        try {
            URI uri = new URI(base);
            return uri.getHost() + (uri.getPort() == -1 ? "" : ":" + uri.getPort());
        } catch (URISyntaxException error) {
            return base;
        }
    }

    private String pairingRequestUrl(String pairingUrl) {
        try {
            String name = (Build.MANUFACTURER + " " + Build.MODEL).trim();
            return pairingUrl + "&device=" + URLEncoder.encode(name, "UTF-8");
        } catch (Exception ignored) {
            return pairingUrl;
        }
    }

    private String cookiePath(String base) {
        try {
            String path = new URI(base).getPath();
            return path == null || path.isEmpty() ? "/" : path;
        } catch (URISyntaxException ignored) {
            return "/";
        }
    }

    private TextView text(String value, int size, int color) {
        TextView view = new TextView(this);
        view.setText(value);
        view.setTextSize(size);
        view.setTextColor(color);
        return view;
    }

    private TextView sectionLabel(String value) {
        TextView view = text(value, 12, TEXT);
        view.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        view.setLetterSpacing(0.08f);
        return view;
    }

    private Button button(String label, int background) {
        Button view = new Button(this);
        view.setText(label);
        view.setTextColor(background == PRIMARY ? PRIMARY_TEXT : TEXT);
        view.setTextSize(13);
        view.setAllCaps(false);
        view.setMinWidth(0);
        view.setMinimumWidth(0);
        view.setMinHeight(dp(40));
        view.setMinimumHeight(dp(40));
        view.setPadding(dp(12), 0, dp(12), 0);
        view.setElevation(0);
        view.setBackground(roundedBackground(background, background == PRIMARY ? PRIMARY : BORDER, 8));
        return view;
    }

    private Button dangerButton(String label) {
        Button view = button(label, DANGER_FILL);
        view.setTextColor(Color.WHITE);
        view.setCompoundDrawablesWithIntrinsicBounds(R.drawable.ic_delete, 0, 0, 0);
        view.setCompoundDrawablePadding(dp(7));
        view.setBackground(roundedBackground(DANGER_FILL, DANGER_BORDER, 8));
        view.setContentDescription("Remove saved desktop");
        return view;
    }

    private GradientDrawable roundedBackground(int fill, int stroke, int radius) {
        GradientDrawable background = new GradientDrawable();
        background.setColor(fill);
        background.setCornerRadius(dp(radius));
        background.setStroke(dp(1), stroke);
        return background;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private final class CompanionWebViewClient extends WebViewClient {
        @Override
        public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
            String url = request.getUrl().toString();
            if ("agents-usage://connections".equalsIgnoreCase(url)) {
                showSetup(null);
                return true;
            }
            if (isAllowed(url)) return false;
            Toast.makeText(MainActivity.this, "Navigation outside the paired desktop was blocked", Toast.LENGTH_SHORT).show();
            return true;
        }

        @Override
        public void onPageStarted(WebView view, String url, android.graphics.Bitmap favicon) {
            mainFrameFailed = false;
            super.onPageStarted(view, url, favicon);
        }

        @Override
        public void onPageFinished(WebView view, String url) {
            super.onPageFinished(view, url);
            if (mainFrameFailed) return;
            if (pendingPairing != null && EndpointParser.belongsToBase(url, pendingPairing.baseUrl)
                    && !urlPathEndsWithPair(url)) {
                pairingSucceeded();
            }
            if (pendingPairing == null) {
                for (String base : endpointBases) {
                    if (EndpointParser.belongsToBase(url, base)) {
                        preferences.edit().putString(LAST_ENDPOINT_KEY, base).apply();
                        attemptedBases.clear();
                        break;
                    }
                }
            }
        }

        @Override
        public void onReceivedError(WebView view, WebResourceRequest request, WebResourceError error) {
            super.onReceivedError(view, request, error);
            if (!request.isForMainFrame()) return;
            mainFrameFailed = true;
            String reason = "Could not reach the desktop: " + error.getDescription();
            if (pendingPairing != null) showSetup(reason);
            else view.post(() -> tryNextEndpoint(reason));
        }

        @Override
        public void onReceivedHttpError(WebView view, WebResourceRequest request, WebResourceResponse response) {
            super.onReceivedHttpError(view, request, response);
            if (!request.isForMainFrame() || response.getStatusCode() < 400) return;
            mainFrameFailed = true;
            String reason = response.getStatusCode() == 401
                    ? "Pairing was rejected. Generate a fresh link on the desktop."
                    : "The desktop returned HTTP " + response.getStatusCode() + ".";
            if (pendingPairing != null) showSetup(reason);
            else view.post(() -> tryNextEndpoint(reason));
        }

        @Override
        public void onReceivedSslError(WebView view, SslErrorHandler handler, SslError error) {
            handler.cancel();
            mainFrameFailed = true;
            String reason = "The desktop certificate could not be verified.";
            if (pendingPairing != null) showSetup(reason);
            else view.post(() -> tryNextEndpoint(reason));
        }

        private boolean urlPathEndsWithPair(String url) {
            try {
                String path = new URI(url).getPath();
                return path != null && path.endsWith("/pair");
            } catch (URISyntaxException error) {
                return true;
            }
        }
    }
}
