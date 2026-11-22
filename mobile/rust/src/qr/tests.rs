use super::base45::{decode, encode};
use super::profile::decode_profile_text;
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443";
const KEY_LEN: usize = 1952;

fn test_key() -> Vec<u8> {
    (0..KEY_LEN).map(|i| i as u8).collect()
}

/// Binary form: one length byte per field, then the fields, then the 1952-byte key.
fn binary(fields: &[&str], key: &[u8]) -> Result<Vec<u8>, std::num::TryFromIntError> {
    let mut out = Vec::new();
    for f in fields {
        out.push(u8::try_from(f.len())?);
    }
    for f in fields {
        out.extend_from_slice(f.as_bytes());
    }
    out.extend_from_slice(key);
    Ok(out)
}

fn profile_text(version: u8, fields: &[&str], key: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    Ok(format!("HARW:PROFILE:{version}:{}", encode(&binary(fields, key)?)))
}

fn is_invalid_profile<T>(result: &Result<T, MobileError>) -> bool {
    matches!(result, Err(MobileError::InvalidProfile { .. }))
}

/// A profile text carrying a real ML-DSA-65 key and node id, so it also passes `ValidatedProfile`.
fn host_profile_text(onion: Option<&str>) -> Result<(String, crate::IdentityMaterial), Box<dyn std::error::Error>> {
    let id = crate::generate_identity("Host".to_string())?;
    let key = hex::decode(&id.enrollment.public_key_hex)?;
    let node = id.enrollment.node_id.clone();
    let text = match onion {
        None => profile_text(1, &["Host", &node, "127.0.0.1:7443"], &key)?,
        Some(o) => profile_text(2, &["Host", &node, "127.0.0.1:7443", o], &key)?,
    };
    Ok((text, id))
}

/// Renders text as a QR frame: EC level M, module scale 4, quiet zone 4 modules, dark 0, light 255,
/// row stride = width + 8 padding bytes. Returns (luma, width, height, row_stride).
fn render(text: &str) -> Result<(Vec<u8>, u32, u32, u32), Box<dyn std::error::Error>> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)?;
    let w = code.width();
    let scale = 4usize;
    let px = (w + 8) * scale;
    let stride = px + 8;
    let mut buf = vec![255u8; stride * px];
    let colors = code.to_colors();
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        buf[((y + 4) * scale + dy) * stride + (x + 4) * scale + dx] = 0;
                    }
                }
            }
        }
    }
    Ok((buf, u32::try_from(px)?, u32::try_from(px)?, u32::try_from(stride)?))
}

#[test]
fn profile_qr_matches_the_host_interoperability_vector() -> TestResult {
    let key = test_key();
    let text = profile_text(1, &["Harw", "gateway-mobile", "127.0.0.1:7443"], &key)?;
    assert_eq!(text.len(), 2996);
    let p = decode_profile_text(&text)?;
    assert_eq!(p.version, 1);
    assert_eq!(p.label, "Harw");
    assert_eq!(p.server_node_id, "gateway-mobile");
    assert_eq!(p.endpoint, "127.0.0.1:7443");
    assert_eq!(p.server_public_key_hex, hex::encode(&key));
    assert_eq!(p.onion, None);
    Ok(())
}

#[test]
fn rejects_non_profile_version_and_malformed_base45() {
    assert!(is_invalid_profile(&decode_profile_text("HARW:PROFILE:3:ABC")));
    assert!(is_invalid_profile(&decode_profile_text("HARW:PROFILE:2:ABC")));
    assert!(is_invalid_profile(&decode_profile_text("HARW:PROFILE:1:::!")));
    assert!(is_invalid_profile(&decode_profile_text("HARW:PROFILE:1::::")));
    assert!(is_invalid_profile(&decode_profile_text("")));
    match decode_profile_text("HARW:ENROLL:1:ABC") {
        Err(MobileError::InvalidProfile { detail }) => assert!(detail.contains("enrollment")),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn base45_uses_the_rfc_vector_and_keeps_whitespace_significant() -> TestResult {
    assert_eq!(encode(b"AB"), "BB8");
    assert_eq!(decode("BB8")?, b"AB");
    assert_eq!(encode(&[36]), " 0");
    assert_eq!(decode(" 0")?, vec![36u8]);
    assert_eq!(encode(b"Hello!!"), "%69 VD92EX0");
    assert_eq!(encode(b"base-45"), "UJCLQE7W581");
    assert_eq!(decode("QED8WEX0")?, b"ietf!");
    assert_eq!(decode("%69 VD92EX0")?, b"Hello!!");
    assert_ne!(decode("%69VD92EX0").ok(), Some(b"Hello!!".to_vec()));
    Ok(())
}

#[test]
fn profile_qr_v2_carries_the_onion_route() -> TestResult {
    let key = test_key();
    let text = profile_text(2, &["Harw", "gateway-mobile", "127.0.0.1:7443", ONION], &key)?;
    assert!(text.len() < 3600);
    let p = decode_profile_text(&text)?;
    assert_eq!(p.onion.as_deref(), Some(ONION));
    assert_eq!(p.version, 1);
    assert_eq!(p.server_public_key_hex, hex::encode(&key));
    Ok(())
}

#[test]
fn profile_qr_v2_rejects_onion_locators_just_outside_the_accepted_lengths() -> TestResult {
    let key = test_key();
    for n in [61usize, 69] {
        let onion = "a".repeat(n);
        let t = profile_text(2, &["Harw", "gateway-mobile", "127.0.0.1:7443", &onion], &key)?;
        assert!(is_invalid_profile(&decode_profile_text(&t)), "{n}");
    }
    Ok(())
}

#[test]
fn profile_qr_v2_accepts_onion_locators_at_the_accepted_length_boundaries() -> TestResult {
    let key = test_key();
    for n in [62usize, 68] {
        let onion = "a".repeat(n);
        let t = profile_text(2, &["Harw", "gateway-mobile", "127.0.0.1:7443", &onion], &key)?;
        assert_eq!(decode_profile_text(&t)?.onion.as_deref(), Some(onion.as_str()));
    }
    Ok(())
}

#[test]
fn profile_qr_v1_round_trips_at_the_maximum_field_lengths() -> TestResult {
    let key = test_key();
    let (l, n, e) = ("L".repeat(128), "n".repeat(128), "e".repeat(63));
    let t = profile_text(1, &[&l, &n, &e], &key)?;
    assert_eq!(t.len() - "HARW:PROFILE:1:".len(), 3411);
    let p = decode_profile_text(&t)?;
    assert_eq!((p.label, p.server_node_id, p.endpoint), (l, n, e));
    assert_eq!(p.server_public_key_hex, hex::encode(&key));
    Ok(())
}

#[test]
fn profile_qr_v2_round_trips_at_the_maximum_field_lengths_below_the_text_limit() -> TestResult {
    let key = test_key();
    let (l, n, e, o) = ("L".repeat(128), "n".repeat(128), "e".repeat(63), "o".repeat(68));
    let t = profile_text(2, &[&l, &n, &e, &o], &key)?;
    assert_eq!(t.len() - "HARW:PROFILE:2:".len(), 3515);
    let p = decode_profile_text(&t)?;
    assert_eq!(p.label, l);
    assert_eq!(p.server_node_id, n);
    assert_eq!(p.endpoint, e);
    assert_eq!(p.onion.as_deref(), Some(o.as_str()));
    assert_eq!(p.server_public_key_hex, hex::encode(&key));
    Ok(())
}

#[test]
fn fingerprint_is_eight_groups_of_four_hex() -> TestResult {
    let fp = fingerprint(&hex::encode([0xabu8; 32]))?;
    let groups: Vec<&str> = fp.split(' ').collect();
    assert_eq!(groups.len(), 8);
    assert!(groups.iter().all(|g| g.len() == 4 && g.chars().all(|c| c.is_ascii_hexdigit())));
    assert!(is_invalid_profile(&fingerprint("zz")));
    Ok(())
}

#[test]
fn decode_pairing_text_validates_a_real_profile() -> TestResult {
    let (text, id) = host_profile_text(None)?;
    let s = decode_pairing_text(text.clone())?;
    assert_eq!(s.profile.server_public_key_hex, id.enrollment.public_key_hex);
    assert_eq!(s.profile.server_node_id, id.enrollment.node_id);
    assert_eq!(s.fingerprint, fingerprint(&s.profile.server_public_key_hex)?);
    assert_eq!(serde_json::from_str::<ServerProfile>(&s.document_json)?, s.profile);
    assert_eq!(s.profile.endpoint, decode_profile_text(&text)?.endpoint);
    Ok(())
}

#[test]
fn decode_pairing_text_validates_a_real_v2_profile() -> TestResult {
    let (text, _) = host_profile_text(Some(ONION))?;
    let s = decode_pairing_text(text)?;
    assert_eq!(s.profile.onion.as_deref(), Some(ONION));
    Ok(())
}

#[test]
fn scan_finds_a_real_profile_in_a_rendered_frame() -> TestResult {
    let (text, id) = host_profile_text(None)?;
    let (buf, w, h, stride) = render(&text)?;
    let s = scan_pairing_frame(buf, w, h, stride)?.ok_or("no profile found in frame")?;
    assert_eq!(s.profile.server_public_key_hex, id.enrollment.public_key_hex);
    Ok(())
}

#[test]
fn scan_returns_none_for_a_blank_frame() -> TestResult {
    assert!(scan_pairing_frame(vec![255u8; 208 * 200], 200, 200, 208)?.is_none());
    Ok(())
}

#[test]
fn scan_returns_none_for_a_non_harw_qr() -> TestResult {
    let (buf, w, h, stride) = render("https://example.org")?;
    assert!(scan_pairing_frame(buf, w, h, stride)?.is_none());
    Ok(())
}

#[test]
fn scan_rejects_an_enrollment_qr() -> TestResult {
    let (buf, w, h, stride) = render("HARW:ENROLL:1:BB8")?;
    assert!(is_invalid_profile(&scan_pairing_frame(buf, w, h, stride)));
    Ok(())
}

#[test]
fn scan_rejects_invalid_frame_dimensions() {
    assert!(is_invalid_profile(&scan_pairing_frame(vec![0u8; 10], 4, 4, 4)));
    assert!(is_invalid_profile(&scan_pairing_frame(vec![], 0, 0, 0)));
    assert!(is_invalid_profile(&scan_pairing_frame(vec![0u8; 100], 10, 10, 9)));
    assert!(is_invalid_profile(&scan_pairing_frame(vec![0u8; 20_000_000], 4097, 4, 4097)));
}
