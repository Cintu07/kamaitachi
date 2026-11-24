# hayate (疾風)

hayate is a terminal client for x and youtube, for chats with claude and chatgpt, and for reading the web. on the bench below it held all four services in 10 tabs in a 10.1 mb peak working set, after scrolling 2,001 rows and streaming a 4,000-token answer. the binary is one 2.4 mb rust executable for windows.

it calls each service's api directly, keeps only the fields a person reads, and draws them with its own renderer. web pages go through the same path as text, with no javascript engine, dom or css. the spec it was built from is `hayate-prd.pdf` (nov 24, 2026), and the web reader came after it.

## numbers

`hayate bench` checks every row of the prd's hard-numbers table. it starts a local replay server in a second process, drives a scripted session through the real network stack over a local socket, and exits 1 if any limit is crossed. this run was a release build on the i7-13650hx laptop, windows 11.

| budget | limit | measured |
|---|---|---|
| binary size, release, stripped | 4 mb | 2.37 mb |
| cold start to first frame | 40 ms | 7.14 ms |
| idle private bytes, no tabs | 4 mb | 3.35 mb |
| idle working set, no tabs (os floor for an empty rust exe) | 4 mb | 6.67 mb (5.56 mb) |
| one text tab, 200 posts | 1 mb | 0.06 mb |
| four services, 10 tabs, images off | 20 mb | 10.12 mb |
| same with image cache full | 30 mb | not measured, images are not built yet |
| keypress to screen updated, p99 | 4 ms | 0.36 ms |
| full-screen redraw 200x60, p99 | 1 ms | 0.39 ms |
| scroll, p99 frame | 8.33 ms (120 fps) | 0.36 ms |
| cpu while idle | 0% | 0 µs of cpu in 10 s |
| heap allocations in 1,000 steady-state frames | 0 | 0 |
| streamed token to screen after it arrives, p99 | 1 ms | 0.27 ms |
| json scanner extra memory per mb parsed | 64 kb | 16.2 kb, 0 allocations |

the idle working-set row cannot pass as written. an empty `fn main() {}` in rust has a 5.5 to 5.7 mb working set on this windows build, almost all of it shared system dll pages, so the bench judges that row on private bytes (the number the prd puts alongside) and prints the floor next to the working set.

the chrome baseline for the same four sites still has to be measured by hand. open x, youtube, claude.ai and chatgpt.com signed in, 10 tabs, and read the total working set of every chrome process in task manager.

## build and run

```
cargo build --release
target\release\hayate.exe
```

it needs windows terminal (or any console with virtual terminal sequences) and the msvc build tools. the first run writes a config to `%APPDATA%\hayate\config.txt`, and history goes to `%LOCALAPPDATA%\hayate`. `hayate config` prints both paths.

secrets never go in the config. they live in windows credential manager under `hayate/<name>`.

```
hayate key set anthropic      claude
hayate key set openai         chatgpt
hayate key set youtube-key    youtube search, comments, channels you list in the config
hayate auth youtube           your subscriptions (google device sign-in)
hayate auth x                 timeline, mentions, posting, likes (oauth 2.0 with pkce)
hayate key list
```

`hayate auth youtube` needs an oauth client of type "tvs and limited input devices" from the google cloud console. `hayate auth x` needs an app in the x developer console with `http://127.0.0.1:8765/callback` as its callback url. video needs mpv, yt-dlp and deno on the path (`winget install mpv yt-dlp.yt-dlp denoland.deno`).

## the web

a new tab opens a start page with a search box. type an address or words, and enter takes you there. words go to duckduckgo's no-javascript page, so the results are real and need no key. pages are read as text in a centered column. hayate keeps headings, paragraphs, lists, code, quotes and links, and skips menus, footers, `[edit]` links and anything a page marks hidden. when a page has a `<main>` or `<article>`, only that part is shown.

tab and shift+tab move between links, enter follows one, and backspace goes back. on link lists like hacker news and search results, the title of each item is bold and the rest of it is dimmed. every page is fetched again on reload because nothing caches pages yet.

## keys

| key | does |
|---|---|
| ctrl+k | the command palette: every action, filtered as you type |
| ctrl+l | go to an address or search the web |
| ctrl+t, ctrl+w | new tab, close tab |
| ctrl+tab, alt+1..9 | next tab, jump to a tab |
| j / k, arrows | next / previous entry, or scroll a page |
| space, page up / down | a screen at a time |
| enter | open: an x post's thread, a youtube video in mpv, a link |
| tab, shift+tab | on a page: next and previous link |
| backspace, L | back, forward |
| c | comments of a youtube video |
| i | the entry's media as a link (images are not drawn yet) |
| r, l, n | reply, like, new post (x); r reloads a page; n is a new chat |
| o | open in the system browser |
| / | search |
| q, u | close (archive) a tab, reopen the last closed one |
| e | export the view to markdown |
| ctrl+c | quit |

in a chat tab the composer has the keyboard. enter sends, shift+enter adds a line, and esc gives the keys back to the view. `:model <id>` and `:system <preset>` work from the palette.

a service with no key or sign-in yet stays out of the tab bar at start. its start-page tile opens a page that says which command connects it.

## how it works

one process, two threads, and mpv as a separate process only while a video plays.

- the net thread sleeps in one `WaitForMultipleObjects` on its command event and every live socket. each host gets one keep-alive tls connection (rustls on ring). http/1.1 is written by hand and parsed as bytes arrive, in any chunk size.
- each adapter declares the json paths it keeps, like `data[*].text`. a macro checks them at compile time and they become a small trie at start. the scanner walks the bytes once, copies only matched values (escapes and html entities decoded on the way) straight into the tab's arena, and skips everything else by counting brackets.
- a tab's text arena is 1 mb of reserved address space, committed 64 kb at a time. the net thread is its only writer. rows (id, author index, time, text span, flags) go to the ui thread through a lock-free spsc ring, and the ui keeps a feed as a struct of arrays. a duplicate post is rolled back out of the arena, so paging over posts already held costs no memory.
- the ui thread waits on the console handle, the reply event and mpv. a frame paints only visible rows into a cell grid of 8-byte cells, diffs it against the last frame by row hash, then cell by cell, and writes the result in one call. grapheme clusters and widths come from unicode 18 data files in `data/ucd`, turned into tables by `build.rs`.
- history is an append-only log per service, made of length-prefixed records with a crc32 each, opened memory-mapped, with a torn tail truncated on start. x posts are never fetched twice. the timeline asks for posts newer than the newest held, and scrolling back reads the log before it asks the api.
- spend is checked before each paid call against the monthly cap in the config, using the worst case (max tokens, posts requested), and the real cost from each response is added to the log.

claude calls go to `/v1/messages` with `anthropic-version: 2026-10-21`, model `claude-opus-5-5` by default, effort `medium`, and the server-side refusal fallback on. chatgpt calls go to the responses api, with storage on openai's side turned off. switching provider in a chat with `:model` starts a new branch that carries the history so far.

## tests

```
cargo test                                  unit tests, golden frames, zero-allocation frame loop, parser fuzzing
cargo test --release --test budgets         the hard-numbers table (release only)
cargo test --release -- --ignored           one request to each real api without credentials (expects 401/403)
hayate bench                                the full budget run with a 10 s idle measurement
```

`hayate shot <folder>` drives the ui through each screen (start page, feeds, chat, hacker news, wikipedia, search, palette) and writes every frame's cells and colors as json. `python tools/shot.py <folder>/*.json` draws them as png with the fonts windows terminal uses, which is how the design was checked.

golden frames live in `tests/golden`, and `HAYATE_BLESS=1 cargo test --test golden` rewrites them after a deliberate change. `HAYATE_RECORD=<dir>` saves raw responses while you use hayate, and `HAYATE_FIXTURES=<dir>` makes the replay server serve them instead of its synthetic ones.

the four parsers that read outside bytes (`http`, `sse`, `json`, `records`) also have cargo-fuzz targets in `fuzz/`, which build on linux and pull in only those modules. a day-long run of one of them under wsl looks like this.

```
rustup toolchain install nightly && cargo install cargo-fuzz
cd fuzz && cargo +nightly fuzz run json -- -max_total_time=86400
```

## not built yet

- images. the prd's sixel and kitty output with a scaled libjpeg-turbo decode is not written, so `i` shows a link to the image and the pixels arena stays empty, which also means the 30 mb image-cache budget has nothing in it to measure.
- real fixtures. the replay server's responses are synthetic, shaped like each api's documented output.
- a non-blocking connect. the net thread opens each host's connection with a blocking tls connect, once per session, and other sockets wait while it does.
- real x prices. `x.price_read` and `x.price_write` in the config are unverified guesses, so set them from the x developer console before you trust `:spend`.
- daylight time. the youtube quota day resets at utc-8.
- the long runs. nobody has fuzzed it for a full day yet, or used it as the only client for 30 days.
