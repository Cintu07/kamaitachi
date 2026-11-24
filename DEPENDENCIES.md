# dependencies

a dependency is allowed only when writing it ourselves would be unsafe or a waste. sizes come from the linker map of the 2.2 mb release build, added up symbol by symbol, and they can be off by a few kb.

| crate | size in the binary | why it is allowed | rule |
|---|---|---|---|
| rustls 0.23 | about 640 kb | tls is where hand-written code gets people hurt | default features off; ring, std, tls 1.2 and 1.3 only |
| ring 0.17 | about 150 kb | constant-time crypto is not a learning project; it was smaller than aws-lc-rs and builds without cmake or nasm | used by rustls, and directly for the sha-256 and randomness in pkce |
| rustls-webpki, rustls-pki-types | about 75 kb | certificate checks for rustls | pulled in by rustls |
| webpki-roots 1.0 | counted with static data | mozilla's root certificates | data only |
| windows-sys 0.61 | 0 kb of code | raw bindings to the console, credential manager, memory and winsock apis | only the feature flags actually called |
| windows 0.62, windows-core | not measured yet | com bindings for direct3d 11, directwrite, wic image decoding and the window | only the feature flags actually called |
| webview2-com 0.39 | not measured yet, plus the 160 kb webview2 loader it embeds | chromium for full-engine tabs, through the webview2 runtime windows already has | nothing of it runs until a page needs the full engine |
| omamori | not measured yet | the ad blocker; it is ours, kept in its own repo | no dependencies of its own |

hayate's own code is about 600 kb. the rest is the rust standard library (about 240 kb) and the msvc runtime with static data such as the root certificates and unicode tables (about 510 kb).

async runtimes are not allowed, and neither are http client crates, json libraries, tui toolkits or logging frameworks. a crate that would pull in a second copy of something already shipped is out too.

images go through wic, the decoder built into windows, so no image crates are needed.

the chrome brings two fonts of its own, cmu typewriter and cmu serif, as woff2 files inside the exe (55 kb together). directwrite unpacks them in memory, so no font crate is needed either.
