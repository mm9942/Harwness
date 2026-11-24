//! Vault PIN profile v1. This is a KDF, not a PIN verifier or vault unlock.
#![forbid(unsafe_code)]

use argon2::{Algorithm, Argon2, Block, Params, Version};
use zeroize::{Zeroize, Zeroizing};

const MIN_PIN_BYTES: usize = 8;
const MAX_PIN_BYTES: usize = 16;
const SALT_BYTES: usize = 16;
const OUTPUT_BYTES: usize = 32;
const SALT_OFFSET: usize = 2 + MAX_PIN_BYTES;
const OUTPUT_OFFSET: usize = SALT_OFFSET + SALT_BYTES;
pub(crate) const FRAME_BYTES: usize = OUTPUT_OFFSET + OUTPUT_BYTES;

fn derive_vault_pin(pin: &[u8], salt: &[u8; SALT_BYTES]) -> Option<Zeroizing<[u8; OUTPUT_BYTES]>> {
    if !(MIN_PIN_BYTES..=MAX_PIN_BYTES).contains(&pin.len()) || !pin.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let params = Params::new(65_536, 3, 4, Some(OUTPUT_BYTES)).ok()?;
    let count = params.block_count();
    let mut memory = Zeroizing::new(Vec::new());
    memory.try_reserve_exact(count).ok()?;
    memory.resize_with(count, Block::default);

    let mut output = Zeroizing::new([0; OUTPUT_BYTES]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(pin, salt, &mut output[..], memory.as_mut_slice())
        .ok()?;
    Some(output)
}

/// Copy the bounded input into zeroizing ownership, then scrub the entire caller frame.
/// Refill only the output range on success. All errors leave the frame zeroed.
pub(crate) fn derive_v1_frame(frame: &mut [u8; FRAME_BYTES]) -> bool {
    let mut input = Zeroizing::new([0u8; OUTPUT_OFFSET]);
    input.copy_from_slice(&frame[..OUTPUT_OFFSET]);
    let clean_output = frame[OUTPUT_OFFSET..].iter().all(|&byte| byte == 0);
    frame.zeroize();

    let len = usize::from(input[1]);
    if !clean_output
        || input[0] != 1
        || !(MIN_PIN_BYTES..=MAX_PIN_BYTES).contains(&len)
        || input[2 + len..SALT_OFFSET].iter().any(|&byte| byte != 0)
    {
        return false;
    }
    let Ok(salt) = <&[u8; SALT_BYTES]>::try_from(&input[SALT_OFFSET..OUTPUT_OFFSET]) else {
        return false;
    };
    let Some(derived) = derive_vault_pin(&input[2..2 + len], salt) else {
        return false;
    };
    frame[OUTPUT_OFFSET..].copy_from_slice(&derived[..]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: [u8; SALT_BYTES] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const KAT: [u8; OUTPUT_BYTES] = [
        0x2f, 0xa2, 0x4d, 0x2e, 0x41, 0x10, 0x68, 0xd6, 0x6b, 0x3a, 0x35, 0xd9, 0xb2, 0x0e, 0x42,
        0x56, 0x88, 0xad, 0x3d, 0x9a, 0xbb, 0x6e, 0xab, 0x6d, 0xe7, 0x69, 0x29, 0x7f, 0x6a, 0x07,
        0x0c, 0x48,
    ];

    #[test]
    fn matches_independent_v1_known_answer_preserving_leading_zero() {
        assert_eq!(derive_vault_pin(b"078631029", &SALT).as_deref(), Some(&KAT));
    }

    #[test]
    fn rejects_invalid_inputs_without_deriving() {
        for invalid in [
            &b"1234567"[..],
            &b"1234567a"[..],
            &b" 12345678"[..],
            &b"12345678901234567"[..],
            &b"12345678\n"[..],
            "９1234567".as_bytes(),
        ] {
            assert!(derive_vault_pin(invalid, &SALT).is_none());
        }
    }

    #[test]
    fn salt_and_leading_zero_are_bound_into_the_kdf() {
        let mut other_salt = SALT;
        other_salt[0] ^= 1;
        let different_salt = derive_vault_pin(b"078631029", &other_salt);
        let missing_zero = derive_vault_pin(b"78631029", &SALT);
        assert!(different_salt.is_some_and(|key| *key != KAT));
        assert!(missing_zero.is_some_and(|key| *key != KAT));
    }

    fn frame(pin: &[u8]) -> [u8; FRAME_BYTES] {
        let mut frame = [0; FRAME_BYTES];
        frame[0] = 1;
        frame[1] = u8::try_from(pin.len()).unwrap();
        frame[2..2 + pin.len()].copy_from_slice(pin);
        frame[SALT_OFFSET..OUTPUT_OFFSET].copy_from_slice(&SALT);
        frame
    }

    #[test]
    fn fixed_frame_returns_only_output() {
        let mut frame = frame(b"078631029");
        assert!(derive_v1_frame(&mut frame));
        assert_eq!(&frame[..OUTPUT_OFFSET], &[0; OUTPUT_OFFSET]);
        assert_eq!(&frame[OUTPUT_OFFSET..], &KAT);
    }

    #[test]
    fn rejects_and_wipes_bad_frame_version_length_padding_input_or_output() {
        for (index, invalid) in [(0, 2), (1, 17), (11, 0x38), (2, 0xff), (OUTPUT_OFFSET, 1)] {
            let mut frame = frame(b"078631029");
            frame[index] = invalid;
            assert!(!derive_v1_frame(&mut frame));
            assert_eq!(frame, [0; FRAME_BYTES]);
        }
    }
}
