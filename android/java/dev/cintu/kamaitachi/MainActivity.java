package dev.cintu.kamaitachi;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.webkit.WebResourceRequest;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.util.ArrayDeque;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The browser window: an address bar on paper, the light engine's page under it, and
 * the System WebView for sites that are really apps. Which engine a site gets is
 * remembered per site, as on Windows.
 */
public final class MainActivity extends Activity {
    // the joseon skin: warm paper, warm black, graphite and cobalt
    static final int PAPER = 0xfff6f2e7;
    static final int DESK = 0xffe8e1d1;
    static final int TEXT = 0xff23211d;
    static final int MUTED = 0xff66615a;
    static final int FAINT = 0xffa39c8e;
    static final int INK = 0xff2a3d91;

    private final ExecutorService loader = Executors.newSingleThreadExecutor();
    private final Handler ui = new Handler(Looper.getMainLooper());
    private final ArrayDeque<String> back = new ArrayDeque<>();
    private EditText address;
    private TextView engine;
    private TextView memory;
    private View loadingLine;
    private PageView pageView;
    private WebView web;
    private String current = "";
    private boolean full;
    /** bumped on every navigation, so a slow load that finishes late is dropped */
    private int seq;
    private SharedPreferences engines;

    private int dp(float v) {
        return (int) TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, getResources().getDisplayMetrics());
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setStatusBarColor(DESK);
        getWindow().setNavigationBarColor(PAPER);
        getWindow().getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
        engines = getSharedPreferences("engines", Context.MODE_PRIVATE);
        Native.start(getFilesDir().getAbsolutePath());

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setBackgroundColor(PAPER);

        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(14), dp(8), dp(10), dp(6));
        bar.setBackgroundColor(PAPER);

        address = new EditText(this);
        address.setSingleLine(true);
        address.setTypeface(Typeface.MONOSPACE);
        address.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        address.setTextColor(INK);
        address.setHintTextColor(FAINT);
        address.setHint("search, or type an address");
        address.setBackground(null);
        address.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        address.setImeOptions(EditorInfo.IME_ACTION_GO);
        address.setSelectAllOnFocus(true);
        address.setOnEditorActionListener((v, id, ev) -> {
            boolean enter = ev != null && ev.getKeyCode() == KeyEvent.KEYCODE_ENTER && ev.getAction() == KeyEvent.ACTION_DOWN;
            if (id == EditorInfo.IME_ACTION_GO || enter) {
                navigate(typed(address.getText().toString()), true);
                hideKeyboard();
                return true;
            }
            return false;
        });
        bar.addView(address, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));

        engine = label("lite", TEXT);
        engine.setPadding(dp(10), dp(6), dp(10), dp(6));
        engine.setOnClickListener(v -> toggleEngine());
        bar.addView(engine);

        memory = label("", MUTED);
        memory.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        memory.setPadding(dp(6), 0, 0, 0);
        bar.addView(memory);
        root.addView(bar);

        // the ruled line under the toolbar; it turns cobalt while a page loads
        loadingLine = new View(this);
        loadingLine.setBackgroundColor(FAINT);
        root.addView(loadingLine, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(1)));

        FrameLayout content = new FrameLayout(this);
        pageView = new PageView(this, href -> navigate(href, true));
        content.addView(pageView);
        web = new WebView(this);
        web.getSettings().setJavaScriptEnabled(true);
        web.getSettings().setDomStorageEnabled(true);
        web.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView v, WebResourceRequest r) {
                return false;
            }

            @Override
            public void onPageFinished(WebView v, String url) {
                if (full) {
                    current = url;
                    showAddress(url);
                }
            }
        });
        web.setVisibility(View.GONE);
        content.addView(web);
        root.addView(content, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        setContentView(root);

        tickMemory();
        Uri data = getIntent().getData();
        if (data != null) navigate(data.toString(), true);
        else address.requestFocus();
    }

    private TextView label(String text, int color) {
        TextView t = new TextView(this);
        t.setText(text);
        t.setTypeface(Typeface.MONOSPACE);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        t.setTextColor(color);
        return t;
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (intent.getData() != null) navigate(intent.getData().toString(), true);
    }

    /** What was typed, as an address: a search unless it looks like one. */
    static String typed(String s) {
        s = s.trim();
        if (s.startsWith("http://") || s.startsWith("https://")) return s;
        if (s.contains(".") && !s.contains(" ")) return "https://" + s;
        return "https://html.duckduckgo.com/html/?q=" + Uri.encode(s);
    }

    private static String host(String url) {
        String h = Uri.parse(url).getHost();
        return h == null ? "" : h.toLowerCase(Locale.ROOT).replaceFirst("^www\\.", "");
    }

    private void showAddress(String url) {
        if (!address.hasFocus()) address.setText(url.replaceFirst("^https://", ""));
    }

    private void navigate(String url, boolean push) {
        if (url.isEmpty()) return;
        if (push && !current.isEmpty()) back.push(current);
        current = url;
        showAddress(url);
        boolean wantsFull = engines.getBoolean(host(url), false);
        if (wantsFull) {
            showFull(url);
            return;
        }
        full = false;
        engine.setText("lite");
        loadingLine.setBackgroundColor(INK);
        final int mine = ++seq;
        final int width = pageView.getWidth() > 0 ? pageView.getWidth() : getResources().getDisplayMetrics().widthPixels;
        final float density = getResources().getDisplayMetrics().density;
        loader.execute(() -> {
            long p = Native.load(url, width, density);
            ui.post(() -> {
                if (mine != seq) {
                    Native.free(p);
                    return;
                }
                loadingLine.setBackgroundColor(FAINT);
                if (Native.needsJs(p)) {
                    String where = Native.url(p);
                    Native.free(p);
                    showFull(where);
                    return;
                }
                current = Native.url(p);
                showAddress(current);
                web.setVisibility(View.GONE);
                web.loadUrl("about:blank");
                pageView.setVisibility(View.VISIBLE);
                pageView.setPage(p);
            });
        });
    }

    /** Opens a site in the System WebView, which runs its JavaScript. */
    private void showFull(String url) {
        full = true;
        ++seq;
        engine.setText("full");
        loadingLine.setBackgroundColor(FAINT);
        pageView.setPage(0);
        pageView.setVisibility(View.GONE);
        web.setVisibility(View.VISIBLE);
        web.loadUrl(url);
    }

    /** Moves this site to the other engine, and remembers that. */
    private void toggleEngine() {
        if (current.isEmpty()) return;
        String h = host(current);
        engines.edit().putBoolean(h, !full).apply();
        navigate(current, false);
    }

    private void hideKeyboard() {
        address.clearFocus();
        InputMethodManager im = getSystemService(InputMethodManager.class);
        if (im != null) im.hideSoftInputFromWindow(address.getWindowToken(), 0);
    }

    private void tickMemory() {
        memory.setText(String.format(Locale.ROOT, "%.0f MB", Native.memoryMb()));
        ui.postDelayed(this::tickMemory, 2000);
    }

    @Override
    public void onBackPressed() {
        if (full && web.canGoBack()) {
            web.goBack();
            return;
        }
        if (!back.isEmpty()) {
            navigate(back.pop(), false);
            return;
        }
        super.onBackPressed();
    }
}
