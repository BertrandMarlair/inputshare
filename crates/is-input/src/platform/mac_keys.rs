//! Translating between a Mac's key codes and a PC's.
//!
//! The wire format carries PC set-1 scan codes, because that is what the
//! keyboard hardware reports and it survives two machines having different
//! keyboard layouts: pressing the key next to Tab sends "the key next to Tab",
//! not the letter it happens to produce on the sender.
//!
//! A Mac numbers its keys differently, so it translates on the way out and on
//! the way back in. Anything missing from this table is simply not forwarded,
//! which is better than forwarding a key nobody pressed.
//!
//! Modifiers are mapped literally: Control is Control and Option is Alt. Not
//! Command. Swapping them would make Ctrl+C copy on a Mac, and would also make
//! Ctrl+C in Terminal stop interrupting anything — a trade that has to be a
//! choice rather than a default.

/// `(mac key code, PC scan code, Windows virtual key, extended)`.
///
/// `extended` is the PC "E0 prefix" flag: the duplicate keys that were added to
/// the keyboard after the scan codes had run out, which is why Home and keypad-7
/// share a number.
const KEYS: &[(u16, u16, u16, bool)] = &[
    // Letters.
    (0, 0x1E, 0x41, false),  // A
    (11, 0x30, 0x42, false), // B
    (8, 0x2E, 0x43, false),  // C
    (2, 0x20, 0x44, false),  // D
    (14, 0x12, 0x45, false), // E
    (3, 0x21, 0x46, false),  // F
    (5, 0x22, 0x47, false),  // G
    (4, 0x23, 0x48, false),  // H
    (34, 0x17, 0x49, false), // I
    (38, 0x24, 0x4A, false), // J
    (40, 0x25, 0x4B, false), // K
    (37, 0x26, 0x4C, false), // L
    (46, 0x32, 0x4D, false), // M
    (45, 0x31, 0x4E, false), // N
    (31, 0x18, 0x4F, false), // O
    (35, 0x19, 0x50, false), // P
    (12, 0x10, 0x51, false), // Q
    (15, 0x13, 0x52, false), // R
    (1, 0x1F, 0x53, false),  // S
    (17, 0x14, 0x54, false), // T
    (32, 0x16, 0x55, false), // U
    (9, 0x2F, 0x56, false),  // V
    (13, 0x11, 0x57, false), // W
    (7, 0x2D, 0x58, false),  // X
    (16, 0x15, 0x59, false), // Y
    (6, 0x2C, 0x5A, false),  // Z
    // Digits.
    (29, 0x0B, 0x30, false), // 0
    (18, 0x02, 0x31, false), // 1
    (19, 0x03, 0x32, false), // 2
    (20, 0x04, 0x33, false), // 3
    (21, 0x05, 0x34, false), // 4
    (23, 0x06, 0x35, false), // 5
    (22, 0x07, 0x36, false), // 6
    (26, 0x08, 0x37, false), // 7
    (28, 0x09, 0x38, false), // 8
    (25, 0x0A, 0x39, false), // 9
    // Punctuation, named by position rather than by what they print.
    (27, 0x0C, 0xBD, false), // minus
    (24, 0x0D, 0xBB, false), // equal
    (33, 0x1A, 0xDB, false), // left bracket
    (30, 0x1B, 0xDD, false), // right bracket
    (41, 0x27, 0xBA, false), // semicolon
    (39, 0x28, 0xDE, false), // quote
    (50, 0x29, 0xC0, false), // grave
    (42, 0x2B, 0xDC, false), // backslash
    (43, 0x33, 0xBC, false), // comma
    (47, 0x34, 0xBE, false), // period
    (44, 0x35, 0xBF, false), // slash
    // Editing and whitespace.
    (36, 0x1C, 0x0D, false), // return
    (48, 0x0F, 0x09, false), // tab
    (49, 0x39, 0x20, false), // space
    (51, 0x0E, 0x08, false), // delete, which a PC calls backspace
    (53, 0x01, 0x1B, false), // escape
    (114, 0x52, 0x2D, true), // help, which a PC calls insert
    (115, 0x47, 0x24, true), // home
    (116, 0x49, 0x21, true), // page up
    (117, 0x53, 0x2E, true), // forward delete
    (119, 0x4F, 0x23, true), // end
    (121, 0x51, 0x22, true), // page down
    (123, 0x4B, 0x25, true), // left
    (124, 0x4D, 0x27, true), // right
    (125, 0x50, 0x28, true), // down
    (126, 0x48, 0x26, true), // up
    // Modifiers. Left and right are distinct keys on both machines.
    (56, 0x2A, 0xA0, false), // shift
    (60, 0x36, 0xA1, false), // right shift
    (59, 0x1D, 0xA2, false), // control
    (62, 0x1D, 0xA3, true),  // right control
    (58, 0x38, 0xA4, false), // option -> alt
    (61, 0x38, 0xA5, true),  // right option -> right alt
    (55, 0x5B, 0x5B, true),  // command -> the Windows key
    (54, 0x5C, 0x5C, true),  // right command
    (57, 0x3A, 0x14, false), // caps lock
    // Function keys.
    (122, 0x3B, 0x70, false), // F1
    (120, 0x3C, 0x71, false), // F2
    (99, 0x3D, 0x72, false),  // F3
    (118, 0x3E, 0x73, false), // F4
    (96, 0x3F, 0x74, false),  // F5
    (97, 0x40, 0x75, false),  // F6
    (98, 0x41, 0x76, false),  // F7
    (100, 0x42, 0x77, false), // F8
    (101, 0x43, 0x78, false), // F9
    (109, 0x44, 0x79, false), // F10
    (103, 0x57, 0x7A, false), // F11
    (111, 0x58, 0x7B, false), // F12
    // Keypad.
    (82, 0x52, 0x60, false), // keypad 0
    (83, 0x4F, 0x61, false), // keypad 1
    (84, 0x50, 0x62, false), // keypad 2
    (85, 0x51, 0x63, false), // keypad 3
    (86, 0x4B, 0x64, false), // keypad 4
    (87, 0x4C, 0x65, false), // keypad 5
    (88, 0x4D, 0x66, false), // keypad 6
    (89, 0x47, 0x67, false), // keypad 7
    (91, 0x48, 0x68, false), // keypad 8
    (92, 0x49, 0x69, false), // keypad 9
    (65, 0x53, 0x6E, false), // keypad decimal
    (67, 0x37, 0x6A, false), // keypad multiply
    (69, 0x4E, 0x6B, false), // keypad plus
    (78, 0x4A, 0x6D, false), // keypad minus
    (75, 0x35, 0x6F, true),  // keypad divide
    (76, 0x1C, 0x0D, true),  // keypad enter
    (71, 0x45, 0x90, false), // keypad clear, which a PC calls num lock
];

/// What to put on the wire for a key this Mac reported.
pub fn to_pc(mac_code: u16) -> Option<(u16, u16, bool)> {
    KEYS.iter()
        .find(|(mac, ..)| *mac == mac_code)
        .map(|&(_, scan, vk, extended)| (scan, vk, extended))
}

/// Which key to press here for a key another machine reported.
///
/// Matches on the scan code first, because that is the physical key. Some scan
/// codes are shared between a keypad key and a navigation key and are told apart
/// by the extended flag, so an exact match is tried before a looser one.
pub fn from_pc(scan: u16, vk: u16, extended: bool) -> Option<u16> {
    if scan != 0 {
        if let Some(&(mac, ..)) = KEYS
            .iter()
            .find(|&&(_, s, _, e)| s == scan && e == extended)
        {
            return Some(mac);
        }
    }
    // A sender that had no scan code to give — an on-screen keyboard, a macro
    // tool — still named the key.
    KEYS.iter()
        .find(|&&(_, _, v, _)| vk != 0 && v == vk)
        .map(|&(mac, ..)| mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_survives_the_round_trip() {
        for &(mac, scan, vk, extended) in KEYS {
            let (out_scan, out_vk, out_extended) =
                to_pc(mac).expect("a key in the table translates out");
            assert_eq!((out_scan, out_vk, out_extended), (scan, vk, extended));
            assert_eq!(
                from_pc(out_scan, out_vk, out_extended),
                Some(mac),
                "key code {mac} came back as a different key"
            );
        }
    }

    #[test]
    fn the_shared_scan_codes_are_told_apart_by_the_extended_flag() {
        // 0x47 is home on a PC keyboard and keypad-7 on the keypad.
        assert_eq!(from_pc(0x47, 0, true), Some(115)); // home
        assert_eq!(from_pc(0x47, 0, false), Some(89)); // keypad 7
                                                       // 0x1C is return, and return again on the keypad.
        assert_eq!(from_pc(0x1C, 0, false), Some(36));
        assert_eq!(from_pc(0x1C, 0, true), Some(76));
    }

    #[test]
    fn a_key_this_mac_has_no_equivalent_for_is_dropped_rather_than_guessed() {
        assert_eq!(to_pc(0xFFFF), None);
        assert_eq!(from_pc(0x00, 0x00, false), None);
    }
}
