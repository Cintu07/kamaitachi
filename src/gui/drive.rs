//! Test renders can use a page before the picture is taken: HAYATE_DO holds steps, each
//! run once the page has loaded. It calls the same functions the window's messages do, so
//! nothing is sent to the desktop and no window is shown.
//!
//! HAYATE_DO="on q; type weasel; key enter" searches. The steps are: `on NAME` (click the
//! control with that name), `click X Y` (page css px from the top of the window's page),
//! `type TEXT`, and `key` with enter, tab, escape, up, down, back, ctrl+a and so on.

use super::*;

impl App {
    /// Runs the next step if the page is ready. Returns whether there was one, or the
    /// page is still loading.
    pub(super) fn drive(&mut self) -> bool {
        if self.steps.is_empty() && !self.driving {
            return self.drove && self.tab().loading;
        }
        // a step draws the window, which asks for the next one
        if self.tab().loading || self.driving {
            return true;
        }
        self.driving = true;
        self.drove = true;
        let step = self.steps.remove(0);
        let (verb, rest) = step.split_once(' ').unwrap_or((step.as_str(), ""));
        match verb {
            "on" => match self.named(rest) {
                Some((x, y)) => self.click(x, y, Mods::default()),
                None => eprintln!("drive: no control called {rest}"),
            },
            "click" => {
                let k = self.k();
                if let Some((x, y)) = rest.split_once(' ').and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?))) {
                    self.click(x * k, TOP * self.scale + y * k, Mods::default());
                }
            }
            "type" => rest.chars().for_each(|c| self.type_char(c)),
            "key" => {
                let (ctrl, name) = rest.strip_prefix("ctrl+").map_or((false, rest), |n| (true, n));
                let (shift, name) = name.strip_prefix("shift+").map_or((false, name), |n| (true, n));
                let vk = match name {
                    "enter" => VK_RETURN,
                    "tab" => VK_TAB,
                    "escape" => VK_ESCAPE,
                    "up" => VK_UP,
                    "down" => VK_DOWN,
                    "left" => VK_LEFT,
                    "right" => VK_RIGHT,
                    "home" => VK_HOME,
                    "end" => VK_END,
                    "back" => VK_BACK,
                    "space" => VK_SPACE,
                    n => VIRTUAL_KEY(n.bytes().next().unwrap_or(b' ').to_ascii_uppercase() as u16),
                };
                self.key(vk, Mods { ctrl, shift, alt: false });
            }
            _ => eprintln!("drive: no step called {verb}"),
        }
        self.driving = false;
        true
    }

    /// The middle of the control with this name, in window px, scrolled into view.
    fn named(&mut self, name: &str) -> Option<(f32, f32)> {
        let t = self.tab();
        let (dl, dom) = (t.dl.as_ref()?, &t.page.as_ref()?.doc.dom);
        let r = dl.items.iter().find_map(|it| match it {
            Item::Field { r, field, .. } if dom.attr(dl.fields[*field as usize].node, crate::web::dom::t::name) == Some(name) => Some(*r),
            _ => None,
        })?;
        let max = self.max_scroll();
        let to = if r.y + r.h > self.view_h() { (r.y - 200.0).clamp(0.0, max) } else { 0.0 };
        let t = self.tab_mut();
        (t.scroll, t.target) = (to, to);
        let k = self.k();
        Some(((r.x + r.w / 2.0) * k, TOP * self.scale + (r.y + r.h / 2.0 - to) * k))
    }
}

/// The steps in HAYATE_DO. Only a test render has any.
pub(super) fn steps() -> Vec<String> {
    if std::env::var_os("HAYATE_SHOT").is_none() {
        return Vec::new();
    }
    std::env::var("HAYATE_DO").unwrap_or_default().split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}
