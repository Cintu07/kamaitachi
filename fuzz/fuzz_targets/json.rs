#![no_main]
#[path = "../../src/json.rs"]
mod json;

json_paths!(P {
    "data[*]" => 1, Record;
    "data[*].id" => 1, Field;
    "data[*].text" => 2, Field;
    "data[*].a.b" => 3, Field;
    "includes.users[*].name" => 4, Field;
    "meta.next_token" => 5, Field;
    "[*].x" => 6, Field;
});

struct Count(usize);
impl json::Sink for Count {
    fn field_start(&mut self, _: u16, _: bool) {}
    fn bytes(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
    fn field_end(&mut self, _: u16) {}
}

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let paths = json::Paths::compile(P);
    let mut sc = json::Scanner::new(&paths);
    let mut c = Count(0);
    for chunk in data.chunks(5) {
        sc.feed(chunk, &mut c);
    }
    sc.finish(&mut c);
    assert!(c.0 <= data.len() * 4);
});
