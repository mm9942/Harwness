use super::*;
use super::base45::{decode, encode};
use super::profile::decode_profile_text;
use crate::{MobileError, ServerProfile};

const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443";

/// Binaerform: je Feld eine u16-BE-Laenge, dann die Felder, dann der 32-Byte-Key.
/// ANNAHME: Laengenkodierung muss mit R1s profile.rs abgeglichen werden.
fn binary(fields: &[&str], key: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in fields {
        out.extend_from_slice(&(f.len() as u16).to_be_bytes());
    }
    for f in fields {
        out.extend_from_slice(f.as_bytes());
    }
    out.extend_from_slice(key);
    out
}

fn profile_text(version: u8, fields: &[&str], key: &[u8]) -> String {
    format!("HARW:PROFILE:{version}:{}", encode(&binary(fields, key)))
}

fn host_profile_text(onion: Option<&str>) -> (String, crate::IdentityMaterial) {
    let id = crate::generate_identity("Host".to_string()).expect("identity");
    let key = hex::decode(&id.enrollment.public_key_hex).expect("hex key");
    let node = id.enrollment.node_id.clone();
    let text = match onion {
        None => profile_text(1, &["Host", &node, "https://host.example:8443"], &key),
        Some(o) => profile_text(2, &["Host", &node, "https://host.example:8443", o], &key),
    };
    (text, id)
}

/// Rendert Text als QR: Skalierung 4, Ruhezone 4 Module, dunkel 0, hell 255.
fn render(text: &str) -> (Vec<u8>, u32, u32) {
    let code = qrcode::QrCode::new(text.as_bytes()).expect("qr");
    let w = code.width();
    let stride = w + 8;
    let scale = 4usize;
    let px = stride * scale;
    let mut buf = vec![255u8; px * px];
    let colors = code.to_colors();
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        buf[((y + 4) * scale + dy) * px + (x + 4) * scale + dx] = 0;
                    }
                }
            }
        }
    }
    (buf, px as u32, px as u32)
}

#[test]
fn profile_qr_matches_the_host_interoperability_vector() {
    // ABWEICHUNG: der feste Host-Vektor (Textlaenge 2996) lag nicht vor; R1 muss ihn einsetzen.
    let (text, id) = host_profile_text(None);
    let p = decode_profile_text(&text).expect("decode");
    assert_eq!(p.server_public_key_hex, id.enrollment.public_key_hex);
    assert_eq!(p.server_node_id, id.enrollment.node_id);
}

#[test]
fn rejects_non_profile_version_and_malformed_base45() {
    let key = [7u8; 32];
    assert!(matches!(
        decode_profile_text(&profile_text(9, &["a", "b", "c"], &key)),
        Err(MobileError::InvalidProfile { .. })
    ));
    assert!(decode_profile_text("HARW:ENROLL:1:AAA").is_err());
    assert!(decode_profile_text("HARW:PROFILE:1:!!!").is_err());
    assert!(decode_profile_text("").is_err());
}

#[test]
fn base45_uses_the_rfc_vector_and_keeps_whitespace_significant() {
    assert_eq!(encode(b"AB"), "BB8");
    assert_eq!(encode(b"Hello!!"), "%69 VD92EX0");
    assert_eq!(encode(b"base-45"), "UJCLQE7W581");
    assert_eq!(decode("QED8WEX0").unwrap(), b"ietf!");
    assert_eq!(decode("%69 VD92EX0").unwrap(), b"Hello!!");
    assert_ne!(decode("%69VD92EX0").ok(), Some(b"Hello!!".to_vec()));
}

#[test]
fn profile_qr_v2_carries_the_onion_route() {
    let (text, _) = host_profile_text(Some(ONION));
    assert!(text.starts_with("HARW:PROFILE:2:"));
    let p = decode_profile_text(&text).unwrap();
    assert_eq!(p.onion.as_deref(), Some(ONION));
    assert_eq!(p.version, 1);
}

#[test]
fn profile_qr_v2_rejects_onion_locators_just_outside_the_accepted_lengths() {
    let key = [1u8; 32];
    for n in [61usize, 69] {
        let onion = "a".repeat(n);
        let t = profile_text(2, &["L", "N", "https://e", &onion], &key);
        assert!(matches!(decode_profile_text(&t), Err(MobileError::InvalidProfile { .. })), "{n}");
    }
}

#[test]
fn profile_qr_v2_accepts_onion_locators_at_the_accepted_length_boundaries() {
    let key = [1u8; 32];
    for n in [62usize, 68] {
        let onion = format!("{}.onion:443", "a".repeat(n - 10));
        assert_eq!(onion.len(), n);
        let t = profile_text(2, &["L", "N", "https://e", &onion], &key);
        assert_eq!(decode_profile_text(&t).unwrap().onion.as_deref(), Some(onion.as_str()));
    }
}

// ANNAHME: Maximallaengen; mit R1s Konstanten abgleichen (Ziel: Base45-Teil 3411 bzw. 3515).
const MAX_LABEL: usize = 64;
const MAX_NODE: usize = 128;
const MAX_ENDPOINT: usize = 2048;

#[test]
fn profile_qr_v1_round_trips_at_the_maximum_field_lengths() {
    let key = [9u8; 32];
    let (l, n, e) = ("l".repeat(MAX_LABEL), "n".repeat(MAX_NODE), "e".repeat(MAX_ENDPOINT));
    let t = profile_text(1, &[&l, &n, &e], &key);
    let p = decode_profile_text(&t).unwrap();
    assert_eq!((p.label, p.server_node_id, p.endpoint), (l, n, e));
    assert_eq!(p.server_public_key_hex, hex::encode(key));
}

#[test]
fn profile_qr_v2_round_trips_at_the_maximum_field_lengths_below_the_text_limit() {
    let key = [9u8; 32];
    let (l, n, e) = ("l".repeat(MAX_LABEL), "n".repeat(MAX_NODE), "e".repeat(MAX_ENDPOINT));
    let onion = format!("{}.onion:443", "a".repeat(58));
    let t = profile_text(2, &[&l, &n, &e, &onion], &key);
    let p = decode_profile_text(&t).unwrap();
    assert_eq!(p.label, l);
    assert_eq!(p.server_node_id, n);
    assert_eq!(p.endpoint, e);
    assert_eq!(p.onion.as_deref(), Some(onion.as_str()));
    assert_eq!(p.server_public_key_hex, hex::encode(key));
    assert_eq!(p.version, 1);
}

#[test]
fn fingerprint_is_eight_groups_of_four_hex() {
    let fp = fingerprint(&hex::encode([0xabu8; 32]));
    let groups: Vec<&str> = fp.split(' ').collect();
    assert_eq!(groups.len(), 8);
    assert!(groups.iter().all(|g| g.len() == 4 && g.chars().all(|c| c.is_ascii_hexdigit())));
}

#[test]
fn decode_pairing_text_validates_a_real_profile() {
    let (text, id) = host_profile_text(None);
    let p: ServerProfile = decode_pairing_text(text.clone()).unwrap();
    assert_eq!(p, decode_profile_text(&text).unwrap());
    assert_eq!(p.server_public_key_hex, id.enrollment.public_key_hex);
}

#[test]
fn decode_pairing_text_validates_a_real_v2_profile() {
    let (text, _) = host_profile_text(Some(ONION));
    let p = decode_pairing_text(text).unwrap();
    assert_eq!(p.onion.as_deref(), Some(ONION));
}

#[test]
fn scan_finds_a_real_profile_in_a_rendered_frame() {
    let (text, id) = host_profile_text(None);
    let (buf, w, h) = render(&text);
    let s: ScannedProfile = scan_pairing_frame(buf, w, h).unwrap().expect("found");
    assert_eq!(s.profile.server_public_key_hex, id.enrollment.public_key_hex);
}

#[test]
fn scan_returns_none_for_a_blank_frame() {
    assert!(scan_pairing_frame(vec![255u8; 200 * 200], 200, 200).unwrap().is_none());
}

#[test]
fn scan_returns_none_for_a_non_harw_qr() {
    let (buf, w, h) = render("https://example.org");
    assert!(scan_pairing_frame(buf, w, h).unwrap().is_none());
}

#[test]
fn scan_rejects_an_enrollment_qr() {
    let (buf, w, h) = render("HARW:ENROLL:1:BB8");
    assert!(matches!(scan_pairing_frame(buf, w, h), Err(MobileError::InvalidProfile { .. })));
}

#[test]
fn scan_rejects_invalid_frame_dimensions() {
    assert!(matches!(scan_pairing_frame(vec![0u8; 10], 4, 4), Err(MobileError::InvalidProfile { .. })));
    assert!(matches!(scan_pairing_frame(vec![], 0, 0), Err(MobileError::InvalidProfile { .. })));
}
