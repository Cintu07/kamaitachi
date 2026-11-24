#![no_main]
// http/1.1 response framing, fed in two pieces split at the first byte's value
#[path = "../../src/net/http.rs"]
mod http;
mod render {
    pub fn push_num(o: &mut Vec<u8>, n: u32) {
        o.extend_from_slice(n.to_string().as_bytes());
    }
}

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let cut = data.first().map_or(0, |&b| b as usize).min(data.len());
    let mut r = http::Response::new();
    let mut n = 0usize;
    for part in [&data[..cut], &data[cut..]] {
        if r.feed(part, &mut |_, _, b| n += b.len()).is_err() {
            return;
        }
    }
    let _ = r.eof();
    assert!(n <= data.len());
});
