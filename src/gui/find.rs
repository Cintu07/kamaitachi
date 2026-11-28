//! Find in page (Ctrl+F). The page is laid out once more with its paragraphs kept, and
//! each match is turned into the boxes it covers by the text engine, which knows where
//! every letter landed. Matches are found again as you type; Enter goes to the next.

use super::*;
use crate::edit::Edit;

pub(super) struct Find {
    pub edit: Edit,
    /// the paragraphs of the page, laid out at its present width
    paras: Vec<(Para, Rect)>,
    tab: u64,
    /// each match, as the boxes it covers in page coordinates
    hits: Vec<Vec<Rect>>,
    at: usize,
}

/// Text lowered one utf-16 unit at a time, so positions stay the same.
fn lower(text: &[u16]) -> Vec<u16> {
    text.iter()
        .map(|&u| {
            let Some(c) = char::from_u32(u as u32) else { return u };
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) if (x as u32) < 0x10000 => x as u16,
                _ => u,
            }
        })
        .collect()
}

/// Where `q` starts in `text`, every time, not overlapping.
fn positions(text: &[u16], q: &[u16]) -> Vec<usize> {
    let mut out = Vec::new();
    if q.is_empty() || text.len() < q.len() {
        return out;
    }
    let mut i = 0;
    while i + q.len() <= text.len() {
        if text[i..i + q.len()] == *q {
            out.push(i);
            i += q.len();
        } else {
            i += 1;
        }
    }
    out
}

impl App {
    pub(super) fn open_find(&mut self) {
        if self.tab().full_mode {
            // Chromium has its own find bar, inside the page
            return self.say("click the page, then Ctrl+F");
        }
        let (vw, vh) = (self.view_w(), self.view_h());
        let c = self.cur;
        let id = self.tabs[c].id;
        let Some(page) = self.tabs[c].page.as_mut() else { return self.say("nothing to find on this page yet") };
        let dl = layout::layout_boxes(&page.doc.dom, &page.styles, &mut self.dw, &mut page.images, &page.doc.base, vw, vh);
        let keep = self.find.take().map(|f| f.edit).unwrap_or_default();
        self.find = Some(Find { edit: keep, paras: dl.paras, tab: id, hits: Vec::new(), at: 0 });
        if let Some(f) = self.find.as_mut() {
            f.edit.select_all();
        }
        self.addr_focused = false;
        self.blur();
        self.search_page();
    }

    fn search_page(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        let q: Vec<u16> = lower(&f.edit.text().encode_utf16().collect::<Vec<_>>());
        let mut hits = Vec::new();
        if !q.is_empty() {
            for (para, r) in &f.paras {
                for at in positions(&lower(&para.text), &q) {
                    let boxes: Vec<Rect> = self.dw.marked(para, r.w, at, at + q.len()).into_iter().map(|m| Rect::new(m.x + r.x, m.y + r.y, m.w, m.h)).collect();
                    if !boxes.is_empty() {
                        hits.push(boxes);
                    }
                }
            }
        }
        // the first match at or below where you are reading
        let top = self.tab().scroll;
        let at = hits.iter().position(|h| h[0].y >= top).unwrap_or(0);
        let f = self.find.as_mut().expect("find is open");
        f.hits = hits;
        f.at = at;
        self.show_hit();
    }

    fn show_hit(&mut self) {
        let Some(r) = self.find.as_ref().and_then(|f| f.hits.get(f.at)).map(|h| h[0]) else { return };
        let (top, h) = (self.tab().scroll, self.view_h());
        if r.y < top + 40.0 || r.y + r.h > top + h - 40.0 {
            self.scroll_to(r.y - h / 3.0);
        }
    }

    fn step_hit(&mut self, back: bool) {
        let Some(f) = self.find.as_mut() else { return };
        let n = f.hits.len();
        if n == 0 {
            return;
        }
        f.at = if back { (f.at + n - 1) % n } else { (f.at + 1) % n };
        self.show_hit();
    }

    /// A key while the find bar is open. Returns whether it used it.
    pub(super) fn find_key(&mut self, vk: VIRTUAL_KEY, m: Mods) -> bool {
        let Some(f) = self.find.as_mut() else { return false };
        if f.tab != self.tabs[self.cur].id {
            self.find = None;
            return false;
        }
        match vk {
            VK_ESCAPE => self.find = None,
            VK_RETURN => self.step_hit(m.shift),
            VK_F3 => self.step_hit(m.shift),
            _ if m.ctrl && vk != VK_A && vk != VK_Z => return false,
            _ => {
                if !fields::edit_key(&mut f.edit, vk, m) {
                    return false;
                }
                self.search_page();
            }
        }
        true
    }

    pub(super) fn find_char(&mut self, c: char) -> bool {
        let Some(f) = self.find.as_mut().filter(|f| f.tab == self.tabs[self.cur].id) else { return false };
        f.edit.insert(&c.to_string());
        self.search_page();
        true
    }

    pub(super) fn find_paste(&mut self, s: &str) -> bool {
        let Some(f) = self.find.as_mut() else { return false };
        f.edit.insert(s.trim());
        self.search_page();
        true
    }

    /// The matches over the page, and the bar at the card's top right.
    pub(super) fn draw_find(&mut self) {
        let Some(f) = self.find.take() else { return };
        if f.tab != self.tab().id {
            self.find = Some(f);
            return;
        }
        let s = sk();
        let (k, (ox, oy)) = (self.k(), self.page_origin());
        let b = self.page_box();
        let scroll = self.tab().scroll;
        let view = [ox, oy, (b.x + b.w) * self.scale, (b.y + b.h) * self.scale];
        let a = s.accent;
        for (i, hit) in f.hits.iter().enumerate() {
            let (fill, alpha) = if i == f.at { (a, 110) } else { ([0xf2, 0xc2, 0x3c, 255], 110) };
            for r in hit {
                let y = (r.y - scroll) * k + oy;
                if y + r.h * k < oy || y > view[3] {
                    continue;
                }
                self.gpu.rect(ox + r.x * k - 1.0, y, r.w * k + 2.0, r.h * k, [fill[0], fill[1], fill[2], alpha], 2.0 * k, view);
            }
        }
        let w = 340.0f32.min(b.w - 24.0);
        let bar = Rect::new(b.x + b.w - w - 14.0, b.y + 12.0, w, 40.0);
        let ks = self.scale;
        self.rect(Rect::new(bar.x - 1.0, bar.y + 3.0, bar.w + 2.0, bar.h), [0, 0, 0, if s.light { 22 } else { 60 }], 11.0);
        self.rect(bar, s.field, 10.0);
        self.gpu.ring(bar.x * ks, bar.y * ks, bar.w * ks, bar.h * ks, 1.0, s.hairline, 10.0 * ks, gpu::NO_CLIP);
        self.icon('\u{E721}', bar.x + 18.0, bar.y + 20.0, 12.0, s.muted);
        let count = if f.edit.is_empty() { String::new() } else if f.hits.is_empty() { "no matches".into() } else { format!("{} of {}", f.at + 1, f.hits.len()) };
        let cw = self.ui_text(&count, 12.0, 400, UI_FONT, s.muted).1;
        let room = bar.w - 44.0 - cw - 16.0;
        let text = f.edit.text();
        if text.is_empty() {
            self.label("Find on this page", bar.x + 34.0, bar.y + 11.0, 14.0, 400, s.faint, room, UI_FONT);
        } else {
            self.label(&text, bar.x + 34.0, bar.y + 11.0, 14.0, 400, s.text, room, UI_FONT);
        }
        if let Some((x0, x1)) = f.edit.selection().map(|(a, z)| {
            let t: String = text.chars().collect();
            let mut w = |n: usize| self.ui_text(&t.chars().take(n).collect::<String>(), 14.0, 400, UI_FONT, s.text).1;
            (w(a), w(z))
        }) {
            self.rect(Rect::new(bar.x + 34.0 + x0, bar.y + 10.0, x1 - x0, 20.0), [a[0], a[1], a[2], 60], 2.0);
        } else {
            let cx = self.ui_text(&f.edit.before(), 14.0, 400, UI_FONT, s.text).1;
            self.rect(Rect::new(bar.x + 34.5 + cx, bar.y + 10.0, 1.5, 20.0), s.ink, 0.5);
        }
        self.label(&count, bar.x + bar.w - 14.0 - cw, bar.y + 12.5, 12.0, 400, if f.hits.is_empty() && !f.edit.is_empty() { s.accent } else { s.muted }, cw + 2.0, UI_FONT);
        self.find = Some(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_without_case() {
        let t: Vec<u16> = "Kamaitachi rides the kamaITACHI wind".encode_utf16().collect();
        let q: Vec<u16> = lower(&"KAMAITACHI".encode_utf16().collect::<Vec<_>>());
        assert_eq!(positions(&lower(&t), &q), [0, 21]);
        assert_eq!(positions(&lower(&t), &[]), Vec::<usize>::new());
        let aa: Vec<u16> = "aaaa".encode_utf16().collect();
        assert_eq!(positions(&aa, &aa[..2]), [0, 2]);
    }
}
