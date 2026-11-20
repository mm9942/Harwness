//! Test-Fehlertyp dieser Crate (siehe `harw-test-support`): ersetzt
//! `panic!`/`unwrap`/`expect` in Tests. Fehler dieser Crate
//! ([`crate::NftError`]) lassen sich mit `?` direkt zurückgeben.

harw_test_support::define_test_error!(pub(crate), Nft(crate::NftError) => "nft-Fehler");
