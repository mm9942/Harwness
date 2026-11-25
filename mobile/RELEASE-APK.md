# Harw Android: OTA vs. GitHub Release

## OTA update channel

`mobile/scripts/release-apk` is the **OTA** helper for the Android update
channel. It edits the Gradle version in the checkout, builds the **debug**
variant unless `--variant release` is passed, and publishes changed bytes
unless `--no-publish` is passed. OTA publication and the release variant both
require the Local Assistant and Library bridge. The script exports
`HARW_LOCAL_UIA=1` to native/Gradle and refuses an explicit opt-out. Only an
unpublished debug build can opt out.
`--no-publish` does not make it a read-only build: it can still edit the
version. The helper stops on a failed `verify-apk` before calling
`publish-update` or advancing the OTA commit marker.

Do not invoke the OTA helper, OTA publisher, or update-service machinery
to stage or distribute a GitHub release APK.

## Stage an Android client for a GitHub release

This is a **separate, manual gate**, not an active Android publishing job.
The existing GitHub release ships Linux/source assets. A Termux package
or cross-build is not proof that the Android client APK installs.

1. Use a clean checkout at the exact authorized release commit. The
   `versionName` and `versionCode` must already be committed there. Use
   the Android SDK/NDK in [PHONE-TEST.md](PHONE-TEST.md). On the trusted
   builder, supply the dedicated release signing configuration through
   the existing `HARW_SIGNING_CONFIG` mechanism; never put it in the
   checkout, receipt, or GitHub assets. Set that and the approved native
   build settings in the environment of **both** build commands.
2. Build the Android client **release** variant directly. This does not
   bump the version, upload to OTA, or publish to GitHub.

   ```sh
   export HARW_LOCAL_UIA=1
   ./mobile/scripts/build-native
   (cd mobile/android && ./gradlew --no-daemon :app:assembleRelease)
   ./mobile/scripts/verify-apk --require-local-uia mobile/android/app/build/outputs/apk/release/app-release.apk
   ```

   Copy the exact signed APK, without re-signing, into private staging
   outside `dist/` as `Harw-<versionName>-build<versionCode>-ARM64.apk`.
   The ordinary verifier does not establish the release signer or
   source provenance; do not stage a debug or unsigned artifact.
3. Independently verify and record exact source commit, APK digest,
   package `dev.harw.mobile`, version code/name, and the release
   certificate fingerprint against the trusted release signer pin.
   On supported Android hardware, record a clean install and an in-place
   update from the pinned predecessor with data retained. In the independently
   pinned receipt, require `smoke.local_assistant_reply = "passed"` and
   `smoke.local_library_persistence = "passed"`: unlock via the real Harw PIN,
   get an on-device reply with a selected GGUF, and reopen an imported Library
   item after restarting the app. Verify access to the room and Library with
   no GGUF selected too. JNI symbols, static checks and successful packaging
   are not device proof.
4. Require the **separately proposed** offline
   `mobile/scripts/admit-release-apk` helper before admitting the APK.
   If that helper has not been integrated, stop; the OTA helper is not
   a substitute. The admission tool compares the staged APK with the
   named commit, independently pinned builder/device receipt, signer
   fingerprint, and predecessor APK hash. Its successful output is the
   APK's separate `.sha256` sidecar:

   ```sh
   sidecar_tmp="$(mktemp -- "${STAGED_APK}.sha256.XXXXXXXX")"
   if python3 -I mobile/scripts/admit-release-apk \
       --apk "$STAGED_APK" --commit "$EXACT_SOURCE_COMMIT" \
       --receipt "$TRUSTED_RECEIPT" --receipt-sha256 "$TRUSTED_RECEIPT_SHA256" \
       --signer-sha256 "$TRUSTED_RELEASE_SIGNER_SHA256" \
       --predecessor-sha256 "$TRUSTED_PREDECESSOR_APK_SHA256" > "$sidecar_tmp"; then
     mv -- "$sidecar_tmp" "${STAGED_APK}.sha256"
   else
     rm -f -- "$sidecar_tmp"
     exit 1
   fi
   ```

Only an explicitly authorized GitHub release action may add those two
files as separate assets, after verifying the exact tagged commit and
required release CI. Leave Linux/source `dist/SHA256SUMS`, archive assets,
and the R2 verification contract unchanged. Nothing in this staging
procedure creates a tag, uploads artifacts, or changes a release.
