# Android PIN and biometric unlock

This handoff records the behavior implemented on PR #170 by
[`afa01433`](https://github.com/mm9942/Harwness/commit/afa01433dcd3427160ed1eb300873ace7b45e542)
and
[`fe20fd50`](https://github.com/mm9942/Harwness/commit/fe20fd5021c6c3a6f7cb6d6fe567ae7611950fa6).
It describes the checked-in implementation, not a certification of a handset
or any later APK. For a failed initial setup, see [Restarting a failed first
PIN setup](PIN-FIRST-SETUP-RECOVERY.md).

## Unlock and fallback

- PIN enrollment remains mandatory. Biometric enrollment is optional and
  scoped to an existing local vault profile. The PIN pad shows masked digits
  and does not persist the entered PIN as UI state.
- `LocalUiaDevRoute.kt` starts the system biometric prompt after the activity
  is resumed, for an eligible, already-enrolled unlock. Cancelling the
  prompt or receiving a terminal failure returns to the Harw PIN without
  opening the prompt again automatically. The PIN screen can offer a manual
  biometric retry. A non-terminal scan mismatch is handled by the system
  prompt, not by the PIN fallback.
- `PinBiometricsSettings.kt` provides **PIN & Biometrics**. Scope changes
  and removal or repair of the optional biometric credential require the
  Harw PIN. The default is vault-only. App entry protection is opt-in and
  uses the selected existing vault profile; it does not encrypt unrelated
  app data. `MainActivity.kt` clears its app-entry grant at `onStop`.
- A destructive local vault reset is a separate, explicitly confirmed
  action, not a recovery step for an otherwise usable PIN. It is blocked
  for the profile currently protecting app entry. Use a disposable local
  profile for the reset test. Do not delete app data or Keystore entries to
  make a recovery option appear.

## Why the Keystore check changed

The app creates a hardware-backed AES-GCM key authorized for each use by
strong biometrics. Android's
[`KeyInfo` API](https://developer.android.com/reference/android/security/keystore/KeyInfo#getUserAuthenticationValidityDurationSeconds())
documents `-1` for per-use authentication.
[Keystore2 parameter encoding](https://github.com/aosp-mirror/platform_frameworks_base/blob/android16-release/keystore/java/android/security/keystore2/KeyStore2ParameterUtils.java#L305-L318)
omits `AUTH_TIMEOUT` when the requested timeout is zero. The [Keystore2
secret-key readback](https://github.com/aosp-mirror/platform_frameworks_base/blob/android16-release/keystore/java/android/security/keystore2/AndroidKeyStoreSecretKeyFactorySpi.java#L75-L90)
initializes the reported duration to zero and [overrides it only if the
timeout tag is present](https://github.com/aosp-mirror/platform_frameworks_base/blob/android16-release/keystore/java/android/security/keystore2/AndroidKeyStoreSecretKeyFactorySpi.java#L166-L175).

The old `-1`-only guard could therefore reject a valid key before the prompt.
`AndroidVaultBiometricSlot.kt` now accepts both `-1` and `0` in this guard.
Hardware-backed enforcement, strong biometric auth, invalidation on changed
enrollment and binding to the prompt's exact `CryptoObject` still apply.
A positive auth window is still rejected. This source-level mismatch
explains a possible pre-prompt failure, not every device-specific failure.

## Existing profiles and verification

1. Verify the candidate's APK signer and package against the installed build.
   Test a same-signer upgrade with an existing test vault, plus a separate
   fresh enrollment. Do not uninstall or reset the existing profile just to
   get the package to install.
2. With a valid existing biometric slot, expect one automatic prompt,
   successful unlock, Back or terminal failure to the PIN pad, and a working
   manual retry. Cover no strong biometric, a failed scan, lockout, and
   changed biometric enrollment separately.
3. A failed provisioning attempt can leave a pending claim or alias. For
   an interrupted or invalid slot, test the PIN-authorized optional-slot
   repair in settings. Confirm a failed cleanup does not allow an old slot
   to reopen, and that the local vault still opens with its original PIN
   before opting in again. Do not confuse this with deleting the vault.
4. In both scopes, test rotation, Home/Recents, process restart and entry
   from a notification or deep link. Check that a callback belonging to an
   obsolete visit does not grant access and that no protected UI appears
   before app-entry authentication.
5. When diagnosing, record only the stage (capability, key metadata, cipher
   initialization, prompt outcome, grant, or session). Never log PIN digits,
   vault material or raw Keystore identifiers.

PIN and biometric verification is separate from Onion transport. Keep
network and gateway changes out of a PIN-only comparison.
