# kamaitachi

kamaitachi is a web browser for Windows that tries to use as little memory as it can. Most pages go through its own engine, which parses the HTML and CSS, lays the page out and paints it on the CPU into a single buffer. Sites that are really applications, like YouTube or Zoom, open in Chromium (the WebView2 runtime that comes with Windows), and only when one of them needs it.

These are private bytes, measured on a Windows 11 laptop in November 2026.

| page | engine | memory |
|---|---|---|
| new tab | own | 7.3 MB |
| Hacker News | own | 9 MB |
| a long Wikipedia article | own | 34 MB |
| a YouTube video page | Chromium | 9 MB + 298 MB |

The budget is 500 MB for both. When the total gets close, the tab you looked at longest ago is parked. Its Chromium process exits and the tab keeps a screenshot, so clicking it brings the page back.

The kamaitachi is a weasel from Japanese folklore that rides whirlwinds and cuts too fast to see.

## build and run

```sh
cargo build --release
target/release/kamaitachi.exe                      # opens a new tab
target/release/kamaitachi.exe news.ycombinator.com  # one tab per address
```

It needs Windows 10 or 11 and Rust 1.88 or newer. The ad blocker, [omamori](https://github.com/Cintu07/omamori), comes from its own repo.

## two engines

A page starts in the light engine. If the page is really a JavaScript application, kamaitachi reopens it in Chromium. It looks for pages with almost no text, a notice asking for JavaScript, or a bot check. That choice is remembered per site in `%APPDATA%\hayate\engines.txt`. The LITE / FULL switch in the toolbar, or `Ctrl+Shift+E`, flips it by hand.

The light engine has no JavaScript at all. It handles block, inline, float, flex, grid and table layout, CSS variables, `calc()` and media queries, and it decodes images at the size they are shown. It does not draw SVG yet, and forms do not submit.

Chromium starts with flags. Each one saved memory on a YouTube video page. Turning off the GPU process took the total from 631 MB to 412 MB, and V8's lite mode cut the page's renderer from 301 MB to 190 MB. Video then decodes on the CPU, which costs battery while it plays. Set `HAYATE_CHROMIUM_ARGS` to replace the flags.

## blocking

Ads, trackers, malware and phishing are blocked by omamori. It reads EasyList, EasyPrivacy, uBlock Origin's own lists, URLhaus and the Phishing URL Blocklist. That is about 300,000 rules, and they take 7.26 MB. The lists are downloaded to `%APPDATA%\hayate\filters` and fetched again after four days.

A page that is on a malware or phishing list does not load at all. You get a warning page with a way back, plus a way through if you know better. `hayate filters` prints the rule counts and memory, and `HAYATE_NOBLOCK=1` turns blocking off.

## keys

| keys | does |
|---|---|
| `Ctrl+L` | address bar, type an address or words to search |
| `Ctrl+T`, `Ctrl+W` | new tab, close tab |
| `Ctrl+Tab`, `Ctrl+1` to `9` | next tab, a tab by number |
| `Alt+Left`, `Alt+Right` | back, forward |
| `Ctrl+Shift+E` | this site in the other engine |
| `Ctrl` `+` / `-` / `0` | zoom |
| `F5` | reload |

Drag an image onto the window and it becomes the new tab's wallpaper.

## settings

| variable | does |
|---|---|
| `HAYATE_BUDGET_MB` | the memory budget, 500 by default |
| `HAYATE_RENDER=gpu` | draw with Direct3D instead of the CPU, which costs about 115 MB of driver memory on an integrated GPU |
| `HAYATE_NOBLOCK=1` | no blocking |
| `HAYATE_CHROMIUM_ARGS` | the flags Chromium starts with |

## hayate

This repo started as hayate, a terminal client for X and YouTube that also talks to Claude and ChatGPT. That program is still here as `hayate.exe`, and the browser's engine grew out of its reader. Setting it up is covered in [docs/hayate.md](docs/hayate.md), along with its keys and memory budgets.

## license

MIT. Filter lists and wallpapers are downloaded when the browser runs, so neither is part of this repo.
