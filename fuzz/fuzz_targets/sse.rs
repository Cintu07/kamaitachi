#![no_main]
#[path = "../../src/net/sse.rs"]
mod sse;

struct Count(usize);
impl sse::SseSink for Count {
    fn begin(&mut self) {}
    fn data(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
    fn dispatch(&mut self) {}
}

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut s = sse::Sse::new();
    let mut c = Count(0);
    for chunk in data.chunks(7) {
        s.feed(chunk, &mut c);
    }
    assert!(c.0 <= data.len() * 2);
});
