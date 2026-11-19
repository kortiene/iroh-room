#![no_main]

use std::str::FromStr;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `from_str` takes text. Invalid UTF-8 never reaches the ticket parser.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = std::hint::black_box(iroh_rooms_core::ticket::RoomInviteTicket::from_str(text));
});
