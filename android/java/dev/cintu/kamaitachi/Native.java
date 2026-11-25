package dev.cintu.kamaitachi;

import android.graphics.Bitmap;

/** The calls into libkamaitachi.so, the same engine the Windows browser uses. */
final class Native {
    static {
        System.loadLibrary("kamaitachi");
    }

    private Native() {}

    /** Starts the ad blocker; it keeps its filter lists under dir. */
    static native void start(String dir);

    /** Loads and lays out a page for a width in device px. Blocks: call it off the main thread. */
    static native long load(String url, int width, float density);

    static native void free(long page);

    static native int height(long page);

    static native String title(long page);

    /** Where the page ended up after redirects. */
    static native String url(long page);

    /** True for a page that is really a JavaScript app, which the WebView should show. */
    static native boolean needsJs(long page);

    /** The link at a point, in device px of the page, or an empty string. */
    static native String linkAt(long page, float x, float y);

    static native float memoryMb();

    /** Draws the page into an ARGB_8888 bitmap, scrolled down by scroll px. */
    static native void draw(long page, Bitmap bitmap, float scroll);
}
