# Restarting a failed first PIN setup

The restart action is not a PIN reset. It applies only to a first-time setup
that stopped before Android and the native room writer established a
recoverable vault. Keep using the existing PIN recovery path when the app
can authenticate a recoverable room. For normal unlock, biometric recovery, and
app-entry protection, see [Android PIN and biometric unlock](ANDROID-PIN-BIOMETRICS.md).

- The failure screen offers **Restart PIN setup** only after both layers
  accept the exact selected profile scope. It asks for confirmation.
- Confirmation repeats the checks under the setup locks. A successful
  restart retires only the old incomplete scope and selects a fresh scope
  for the PIN form. It does not delete any file, alias, or key.
- An existing or possibly recoverable vault, an already-charged recovery
  attempt, or an ambiguous journal makes the action unavailable. Do not
  delete app data or keys to make the action appear.
- If confirmation cannot establish the new scope, reopen the Assistant.
  The app reopens only the durably selected scope; it never replays the
  new setup automatically.

## Sharing a diagnosis

On a setup failure, use **Copy diagnostic code** to share only the fixed
`PIN-xx` code. Do not send your PIN, profile identifiers, or vault files.
`PIN-15` means Harw could not confirm that it saved the final enrollment
record. Reopen the Assistant and follow the action actually displayed.
`PIN-17` instead means the PIN step finished, but the Assistant could not
open the authenticated private session. Reopen the Assistant and use your
existing PIN. This is not an invitation to restart setup or delete app data.

## Conditions to verify before releasing

- An early failed setup with no committed or staged native vault offers
  restart; declining leaves the profile and vault unchanged.
- A recoverable staged room still offers same-PIN continuation, not
  restart. An existing ready room opens normally and never offers restart.
- A competing setup or recovery, unknown directory entry, or ambiguous
  state fails closed.
- Interruption after retirement but before switching the profile allows an
  explicit recovery of the operation. Neither the old scope nor an
  unconfirmed new profile mapping can open for a second enrollment.
- A successful restart returns to the new-PIN screen and keeps all old
  files and Keystore entries intact.

These are release-verification conditions, not claims about any particular
device or a shipped build.
