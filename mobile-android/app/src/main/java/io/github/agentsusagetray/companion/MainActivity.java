package io.github.agentsusagetray.companion;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.res.ColorStateList;
import android.graphics.Color;
import android.net.Uri;
import android.net.http.SslError;
import android.os.Bundle;
import android.os.Build;
import android.window.OnBackInvokedDispatcher;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
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
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import org.json.JSONArray;
import org.json.JSONException;

import java.net.URI;
import java.net.URISyntaxException;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

public final class MainActivity extends Activity {
    private static final String PREFS = "connections";
    private static final String ENDPOINTS_KEY = "endpoint_bases";
    private static final String LAST_ENDPOINT_KEY = "last_endpoint";
    private static final int BACKGROUND = Color.rgb(17, 19, 21);
    private static final int SURFACE = Color.rgb(31, 34, 37);
    private static final int PRIMARY = Color.rgb(39, 191, 206);
    private static final int TEXT = Color.rgb(244, 247, 248);
    private static final int MUTED = Color.rgb(174, 181, 184);
    private static final int DANGER = Color.rgb(239, 100, 100);

    private final List<String> endpointBases = new ArrayList<>();
    private final Set<String> attemptedBases = new LinkedHashSet<>();
    private SharedPreferences preferences;
    private FrameLayout root;
    private WebView webView;
    private ScrollView setupView;
    private EditText pairingInput;
    private TextView noticeView;
    private LinearLayout endpointList;
    private EndpointParser.ParsedEndpoint pendingPairing;
    private boolean mainFrameFailed;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().setStatusBarColor(BACKGROUND);
        getWindow().setNavigationBarColor(BACKGROUND);
        preferences = getSharedPreferences(PREFS, MODE_PRIVATE);
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
        CookieManager.getInstance().flush();
        super.onPause();
    }

    @Override
    protected void onDestroy() {
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

        webView = new WebView(this);
        webView.setBackgroundColor(BACKGROUND);
        root.addView(webView, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        LinearLayout content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        content.setPadding(dp(24), dp(28), dp(24), dp(32));

        TextView eyebrow = text("PHONE COMPANION", 12, PRIMARY);
        eyebrow.setLetterSpacing(0.14f);
        content.addView(eyebrow);

        TextView title = text("Connect to your desktop", 28, TEXT);
        title.setPadding(0, dp(9), 0, 0);
        content.addView(title);

        TextView explanation = text(
                "On the desktop, enable mobile access and generate a private pairing link. Paste it here. "
                        + "You can add both LAN and Tailscale links; the app will use whichever is reachable.",
                15, MUTED);
        explanation.setLineSpacing(dp(3), 1.0f);
        explanation.setPadding(0, dp(12), 0, dp(18));
        content.addView(explanation);

        pairingInput = new EditText(this);
        pairingInput.setHint("https://desktop.example.ts.net/agents-usage/pair?token=…");
        pairingInput.setHintTextColor(Color.rgb(120, 128, 132));
        pairingInput.setTextColor(TEXT);
        pairingInput.setTextSize(14);
        pairingInput.setSingleLine(false);
        pairingInput.setMinLines(2);
        pairingInput.setMaxLines(4);
        pairingInput.setGravity(Gravity.TOP | Gravity.START);
        pairingInput.setPadding(dp(14), dp(12), dp(14), dp(12));
        pairingInput.setBackgroundTintList(ColorStateList.valueOf(PRIMARY));
        content.addView(pairingInput, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        LinearLayout actions = new LinearLayout(this);
        actions.setOrientation(LinearLayout.HORIZONTAL);
        actions.setGravity(Gravity.END);
        actions.setPadding(0, dp(10), 0, 0);
        Button paste = button("Paste", SURFACE);
        paste.setOnClickListener(view -> pastePairingLink());
        actions.addView(paste);
        Button connect = button("Pair", PRIMARY);
        LinearLayout.LayoutParams connectParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        connectParams.setMargins(dp(8), 0, 0, 0);
        actions.addView(connect, connectParams);
        connect.setOnClickListener(view -> beginPairing(pairingInput.getText().toString()));
        content.addView(actions);

        noticeView = text("", 14, DANGER);
        noticeView.setPadding(0, dp(12), 0, 0);
        noticeView.setVisibility(View.GONE);
        content.addView(noticeView);

        TextView savedTitle = text("SAVED CONNECTIONS", 12, MUTED);
        savedTitle.setLetterSpacing(0.12f);
        savedTitle.setPadding(0, dp(30), 0, dp(9));
        content.addView(savedTitle);

        endpointList = new LinearLayout(this);
        endpointList.setOrientation(LinearLayout.VERTICAL);
        content.addView(endpointList);

        TextView help = text(
                "The token is exchanged for a protected desktop cookie and is never saved in this list. "
                        + "Press Android Back while viewing usage to manage connections.",
                13, MUTED);
        help.setLineSpacing(dp(2), 1.0f);
        help.setPadding(0, dp(24), 0, 0);
        content.addView(help);

        setupView = new ScrollView(this);
        setupView.setFillViewport(true);
        setupView.setBackgroundColor(BACKGROUND);
        setupView.addView(content);
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
            pendingPairing = EndpointParser.parse(link);
        } catch (IllegalArgumentException error) {
            showSetup(error.getMessage());
            return;
        }
        hideKeyboard();
        attemptedBases.clear();
        mainFrameFailed = false;
        setupView.setVisibility(View.GONE);
        webView.setVisibility(View.VISIBLE);
        webView.loadUrl(pendingPairing.pairingUrl);
    }

    private void connectToPreferredEndpoint() {
        if (endpointBases.isEmpty()) {
            showSetup(null);
            return;
        }
        String preferred = preferences.getString(LAST_ENDPOINT_KEY, endpointBases.get(0));
        attemptedBases.clear();
        pendingPairing = null;
        if (!endpointBases.contains(preferred)) preferred = endpointBases.get(0);
        loadEndpoint(preferred);
    }

    private void loadEndpoint(String baseUrl) {
        attemptedBases.add(baseUrl);
        mainFrameFailed = false;
        setupView.setVisibility(View.GONE);
        webView.setVisibility(View.VISIBLE);
        webView.loadUrl(baseUrl);
    }

    private void tryNextEndpoint(String reason) {
        for (String base : endpointBases) {
            if (!attemptedBases.contains(base)) {
                loadEndpoint(base);
                return;
            }
        }
        showSetup(reason == null ? "None of the saved desktops could be reached." : reason);
    }

    private void pairingSucceeded() {
        String baseUrl = pendingPairing.baseUrl;
        if (!endpointBases.contains(baseUrl)) endpointBases.add(0, baseUrl);
        preferences.edit().putString(LAST_ENDPOINT_KEY, baseUrl).apply();
        saveEndpoints();
        pendingPairing = null;
        pairingInput.setText("");
        CookieManager.getInstance().flush();
        webView.clearHistory();
        Toast.makeText(this, "Desktop paired", Toast.LENGTH_SHORT).show();
    }

    private void showSetup(String message) {
        pendingPairing = null;
        webView.stopLoading();
        webView.setVisibility(View.GONE);
        setupView.setVisibility(View.VISIBLE);
        if (message == null || message.isBlank()) {
            noticeView.setVisibility(View.GONE);
        } else {
            noticeView.setText(message);
            noticeView.setVisibility(View.VISIBLE);
        }
        renderEndpoints();
    }

    private void renderEndpoints() {
        endpointList.removeAllViews();
        if (endpointBases.isEmpty()) {
            TextView empty = text("No desktops paired yet.", 14, MUTED);
            empty.setPadding(0, dp(8), 0, dp(8));
            endpointList.addView(empty);
            return;
        }
        for (String base : new ArrayList<>(endpointBases)) {
            LinearLayout row = new LinearLayout(this);
            row.setGravity(Gravity.CENTER_VERTICAL);
            row.setPadding(dp(12), dp(9), dp(6), dp(9));
            row.setBackgroundColor(SURFACE);

            LinearLayout labels = new LinearLayout(this);
            labels.setOrientation(LinearLayout.VERTICAL);
            TextView host = text(displayHost(base), 15, TEXT);
            TextView url = text(base, 12, MUTED);
            labels.addView(host);
            labels.addView(url);
            row.addView(labels, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));

            Button use = button("Use", PRIMARY);
            use.setOnClickListener(view -> {
                attemptedBases.clear();
                pendingPairing = null;
                loadEndpoint(base);
            });
            row.addView(use);
            Button remove = button("Remove", SURFACE);
            remove.setTextColor(DANGER);
            remove.setOnClickListener(view -> removeEndpoint(base));
            row.addView(remove);

            LinearLayout.LayoutParams rowParams = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            rowParams.setMargins(0, 0, 0, dp(8));
            endpointList.addView(row, rowParams);
        }
    }

    private void removeEndpoint(String base) {
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

    private void pastePairingLink() {
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

    private TextView text(String value, int size, int color) {
        TextView view = new TextView(this);
        view.setText(value);
        view.setTextSize(size);
        view.setTextColor(color);
        return view;
    }

    private Button button(String label, int background) {
        Button view = new Button(this);
        view.setText(label);
        view.setTextColor(background == PRIMARY ? Color.rgb(7, 32, 35) : TEXT);
        view.setTextSize(13);
        view.setAllCaps(false);
        view.setMinWidth(0);
        view.setMinimumWidth(0);
        view.setPadding(dp(12), 0, dp(12), 0);
        view.setBackgroundTintList(ColorStateList.valueOf(background));
        return view;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private final class CompanionWebViewClient extends WebViewClient {
        @Override
        public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
            String url = request.getUrl().toString();
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
