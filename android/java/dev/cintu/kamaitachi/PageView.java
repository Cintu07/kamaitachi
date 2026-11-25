package dev.cintu.kamaitachi;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.View;
import android.widget.OverScroller;

/**
 * Shows a page from the light engine. The page is drawn by Rust straight into one
 * Bitmap the size of this view; scrolling just draws it again further down.
 */
final class PageView extends View {
    interface Links {
        void open(String href);
    }

    private long page;
    private float scroll;
    private Bitmap bitmap;
    private final OverScroller scroller;
    private final GestureDetector gestures;
    private final Links links;

    PageView(Context c, Links links) {
        super(c);
        this.links = links;
        scroller = new OverScroller(c);
        gestures = new GestureDetector(c, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                scroller.forceFinished(true);
                return true;
            }

            @Override
            public boolean onScroll(MotionEvent a, MotionEvent b, float dx, float dy) {
                scrollTo(scroll + dy);
                return true;
            }

            @Override
            public boolean onFling(MotionEvent a, MotionEvent b, float vx, float vy) {
                scroller.fling(0, (int) scroll, 0, (int) -vy, 0, 0, 0, maxScroll());
                postInvalidateOnAnimation();
                return true;
            }

            @Override
            public boolean onSingleTapUp(MotionEvent e) {
                if (page == 0) return false;
                String href = Native.linkAt(page, e.getX(), e.getY() + scroll);
                if (!href.isEmpty()) links.open(href);
                return true;
            }
        });
    }

    /** Shows a new page and lets the old one go. */
    void setPage(long p) {
        if (page != 0) Native.free(page);
        page = p;
        scroll = 0;
        scroller.forceFinished(true);
        invalidate();
    }

    long page() {
        return page;
    }

    private int maxScroll() {
        return page == 0 ? 0 : Math.max(0, Native.height(page) - getHeight());
    }

    private void scrollTo(float y) {
        scroll = Math.max(0, Math.min(y, maxScroll()));
        invalidate();
    }

    @Override
    protected void onSizeChanged(int w, int h, int ow, int oh) {
        if (w > 0 && h > 0) bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888);
    }

    @Override
    public boolean onTouchEvent(MotionEvent e) {
        return gestures.onTouchEvent(e) || super.onTouchEvent(e);
    }

    @Override
    public void computeScroll() {
        if (scroller.computeScrollOffset()) {
            scroll = scroller.getCurrY();
            postInvalidateOnAnimation();
        }
    }

    @Override
    protected void onDraw(Canvas canvas) {
        if (page == 0 || bitmap == null) {
            canvas.drawColor(0xfff6f2e7);
            return;
        }
        Native.draw(page, bitmap, scroll);
        canvas.drawBitmap(bitmap, 0, 0, null);
    }
}
