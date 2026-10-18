#![no_main]

use libfuzzer_sys::fuzz_target;

// The doc.dat object stream and the model built from it, wrapped in a stored archive entry so
// mutations reach the tagged grammar instead of the archive's checksum.
fuzz_target!(|stream: &[u8]| {
    let _ = terrazon_affinity::stream::parse(stream);
    let file = terrazon_affinity::synth::container(&[("doc.dat", stream, terrazon_affinity::synth::Method::Stored)], None);
    let _ = terrazon_affinity::read(&file, terrazon_affinity::Limits { max_entry: 16 << 20, max_total: 64 << 20 });
});
