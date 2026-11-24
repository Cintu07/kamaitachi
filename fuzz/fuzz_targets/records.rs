#![no_main]
// the log reader: length-prefixed, checksummed records, then field decoding
#[path = "../../src/records.rs"]
mod records;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // half the time, wrap the input in a valid record so field decoding gets exercised
    let framed;
    let input = if data.first().is_some_and(|b| b & 1 == 1) {
        let mut v = (data.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(&records::crc32(data).to_le_bytes());
        v.extend_from_slice(data);
        framed = v;
        &framed[..]
    } else {
        data
    };
    for (_, p) in records::records(input) {
        let mut r = records::RecReader::new(p);
        while r.u8().is_some() {
            let _ = r.str();
            let _ = r.u32();
            let _ = r.u64();
        }
    }
});
