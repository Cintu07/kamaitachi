//! Hiding a box on a page by pointing at it (Ctrl+Shift+H), and the reading view
//! (Ctrl+Shift+R). Both work on the light engine's own copy of the page.
//!
//! While picking, the page is laid out once more with every box kept, so the box under
//! the mouse is known. The arrow keys or the wheel move out to the box around it and
//! back in. A click hides it on that site for good; Backspace brings back the last one.

use super::*;
use crate::web::{hide, read};

pub(super) struct Pick {
    boxes: Vec<(u32, Rect)>,
    /// the box picked, and the ones the pick moved out from
    at: Vec<usize>,
    host: String,
}

impl App {
    fn lite_page(&self) -> bool {
        let t = self.tab();
        t.page.is_some() && !t.full_mode
    }

    pub(super) fn start_pick(&mut self) {
        if !self.lite_page() {
            return self.say("hiding works on pages in lite");
        }
        self.blur();
        let host = host_of(&self.tab().url);
        self.picking = Some(Pick { boxes: Vec::new(), at: Vec::new(), host });
        self.measure_boxes();
        self.say("click what to hide · arrows for a bigger box · backspace undoes · esc");
    }

    fn measure_boxes(&mut self) {
        let (vw, vh) = (self.view_w(), self.view_h());
        let c = self.cur;
        let Some(page) = self.tabs[c].page.as_mut() else { return };
        let dl = layout::layout_boxes(&page.doc.dom, &page.styles, &mut self.dw, &mut page.images, &page.doc.base, vw, vh);
        let dom = &page.doc.dom;
        let boxes = dl.boxes.into_iter().filter(|(n, r)| r.w > 2.0 && r.h > 2.0 && !matches!(dom.tag(*n), crate::web::dom::t::html | crate::web::dom::t::body)).collect();
        if let Some(p) = self.picking.as_mut() {
            p.boxes = boxes;
            p.at.clear();
        }
    }

    /// Picks the innermost box under the mouse. Returns whether picking is on.
    pub(super) fn pick_hover(&mut self, x: f32, y: f32) -> bool {
        if self.picking.is_none() {
            return false;
        }
        let (px, py) = self.to_page(x, y);
        if let Some(p) = self.picking.as_mut() {
            let under = p.boxes.iter().position(|(_, r)| r.contains(px, py));
            // still over the same box: keep the bigger one the arrows may have chosen
            if p.at.first() != under.as_ref() {
                p.at = under.into_iter().collect();
            }
        }
        self.render();
        true
    }

    /// Out to the box around the one picked, or back in.
    fn pick_step(&mut self, out: bool) {
        let Some(dom) = self.tab().page.as_ref().map(|p| &p.doc.dom) else { return };
        let Some(p) = self.picking.as_ref() else { return };
        let Some(&cur) = p.at.last() else { return };
        if !out {
            if p.at.len() > 1 && let Some(p) = self.picking.as_mut() {
                p.at.pop();
            }
            return;
        }
        let mut n = dom.parent(p.boxes[cur].0);
        while n != crate::web::dom::NONE {
            if let Some(i) = p.boxes.iter().position(|b| b.0 == n) {
                if let Some(p) = self.picking.as_mut() {
                    p.at.push(i);
                }
                return;
            }
            n = dom.parent(n);
        }
    }

    pub(super) fn pick_wheel(&mut self, up: bool) -> bool {
        if self.picking.is_none() {
            return false;
        }
        self.pick_step(up);
        self.render();
        true
    }

    pub(super) fn pick_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        if self.picking.is_none() {
            return false;
        }
        match vk {
            VK_ESCAPE => self.picking = None,
            VK_UP | VK_LEFT => self.pick_step(true),
            VK_DOWN | VK_RIGHT => self.pick_step(false),
            VK_RETURN => self.pick_hide(),
            VK_BACK => {
                let host = self.picking.as_ref().map(|p| p.host.clone()).unwrap_or_default();
                if hide::undo(&host) {
                    self.rehide(&host);
                    self.say("brought back");
                } else {
                    self.say("nothing hidden here");
                }
            }
            _ => return false,
        }
        true
    }

    /// A click while picking. Returns whether picking took it.
    pub(super) fn pick_click(&mut self) -> bool {
        if self.picking.is_none() {
            return false;
        }
        self.pick_hide();
        self.render();
        true
    }

    fn pick_hide(&mut self) {
        let Some(p) = self.picking.as_ref() else { return };
        let Some(&i) = p.at.last() else { return };
        let (node, host) = (p.boxes[i].0, p.host.clone());
        let Some(page) = self.tab().page.as_ref() else { return };
        let sel = hide::selector(&page.doc.dom, node);
        hide::add(&host, &sel);
        self.rehide(&host);
        let n = hide::count(&host);
        self.say(&format!("hidden on {} · {n} there now · backspace undoes", host.trim_start_matches("www.")));
    }

    /// Puts this site's hiding rules into the page again and lays it out.
    fn rehide(&mut self, host: &str) {
        let Some(page) = self.tab_mut().page.as_mut() else { return };
        let i = page.doc.hidden;
        if let Some(s) = page.doc.sheets.get_mut(i) {
            *s = Arc::new(crate::web::css::parse_sheet(&hide::css(host)));
        }
        self.restyle();
        self.measure_boxes();
    }

    /// The box picked, drawn over the page.
    pub(super) fn draw_pick(&mut self) {
        let Some(p) = self.picking.as_ref() else { return };
        let Some(&i) = p.at.last() else { return };
        let (node, r) = p.boxes[i];
        let (k, s) = (self.k(), sk());
        let (ox, top) = self.page_origin();
        let pb = self.page_box();
        let y = (r.y - self.tab().scroll) * k + top;
        let x = ox + r.x * k;
        let view = [ox, top, (pb.x + pb.w) * self.scale, (pb.y + pb.h) * self.scale];
        let a = s.accent;
        self.gpu.rect(x, y, r.w * k, r.h * k, [a[0], a[1], a[2], 46], 2.0 * k, view);
        self.gpu.ring(x, y, r.w * k, r.h * k, 2.0 * k, s.ink, 2.0 * k, view);
        let name = self.tab().page.as_ref().map_or(String::new(), |pg| crate::web::dom::name(pg.doc.dom.tag(node)).to_string());
        let tag = format!("{name}  {:.0} × {:.0}", r.w, r.h);
        let lx = (x / self.scale).max(pb.x + 4.0);
        let ly = ((y / self.scale) - 22.0).max(pb.y + 2.0);
        let w = self.ui_text(&tag, 11.5, 400, MONO, s.text).1 + 14.0;
        self.rect(Rect::new(lx, ly, w, 19.0), s.ink, 3.0);
        self.label(&tag, lx + 7.0, ly + 2.5, 11.5, 400, s.field, w, MONO);
    }

    // ---- reading view ----

    pub(super) fn toggle_reading(&mut self) {
        let c = self.cur;
        if self.tabs[c].reading {
            let url = self.tabs[c].url.clone();
            return self.load(c, url, false);
        }
        if !self.lite_page() {
            return self.say("reading view works on pages in lite");
        }
        let s = sk();
        let colors = [hex(s.field), hex(s.text), hex(s.ink), hex(s.muted)];
        let Some(html) = self.tab().page.as_ref().and_then(|p| read::view(&p.doc, text::SERIF, colors)) else {
            return self.say("no article on this page");
        };
        let title = self.tab().title.clone();
        self.show_html(c, &html, &title);
        let t = &mut self.tabs[c];
        // its links are the page's, never the browser's own
        t.own = false;
        t.reading = true;
    }

    // ---- sound ----

    pub(super) fn toggle_mute(&mut self) {
        let c = self.cur;
        let Some(f) = &self.tabs[c].full else { return self.say("pages in lite make no sound") };
        let on = !self.tabs[c].muted;
        full::mute(&f.wv, on);
        self.tabs[c].muted = on;
        self.say(if on { "muted" } else { "sound on" });
    }
}
