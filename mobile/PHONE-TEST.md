# Harw Android phone test

The arm64 debug APK uses the same pinned node handshake and hosted-session wire protocol as the Harw gateway. Android Keystore protects the phone identity. The gateway runs with a dedicated home and its own AuthHub. It listens on `127.0.0.1` for USB tests or on its assigned Tailscale address when started with `--tailnet`. The phone's enrollment and the server profile are public JSON documents. Verify the source of the profile before importing it because it supplies the server identity pin. [Public file formats](INTERFACE.md)

## Build and check the APK

On the build host, use the repository's Rust toolchain, the `aarch64-linux-android` target, `cargo-ndk`, JDK 17 or newer, Android SDK 37.1 with Build Tools 37.0.0 and NDK 28.2.13676358. Run from the repository root.

```sh
export ANDROID_HOME="$HOME/Android/Sdk"
./mobile/scripts/build-apk
```

This builds the arm64 library, generates Kotlin bindings from that library, runs the Android unit tests, assembles the debug APK, checks its signature and 16 KB zip alignment, and prints its SHA-256. The APK is `mobile/android/app/build/outputs/apk/debug/app-debug.apk`. Set `MOBILE_OFFLINE=1` on a host with the required dependencies and Gradle distribution already cached.

## Build the server launcher

On the server, use the installed `harw` that supports `[session_listener]` and the `harw-auth-hub` executable. If the AuthHub executable is available only in a checkout build, pass its exact path with `--auth-hub`. Smoke mode needs Python 3 on the server.

```sh
cargo build --locked --manifest-path mobile/rust/Cargo.toml \
  --features bootstrap --bin mobile-bootstrap
BOOTSTRAP="$PWD/mobile/rust/target/debug/mobile-bootstrap"
```

The launcher builds the onion dialer by default, so the phone's Tor (.onion) route needs no extra launcher feature. The gateway it starts still needs the `onion` build described in [Connect over Tor](#connect-over-tor-onion).

If you set `CARGO_TARGET_DIR`, use the executable in that target directory. You can run the connection and reply check on the server without a phone. It starts a fresh test gateway, verifies that a wrong pin blocks session access, then connects with the correct pin and verifies replies in two independently attached sessions.

```sh
"$BOOTSTRAP" smoke-test --harw "$(command -v harw)" \
  --auth-hub "$PWD/target/debug/harw-auth-hub"
```

## Start a USB test from one terminal

On a Linux computer with the gateway binaries, built APK, and connected USB phone, run the helper below. Enable USB debugging on the phone and authorize this computer first.

```sh
./mobile/scripts/usb-test --chatgpt-model "$CHATGPT_MODEL"
```

The helper installs and opens the app, waits for the public enrollment export, starts the gateway, configures the USB port and copies the public server profile. Import the profile in the app and tap **Connect**. Leave the terminal running for the test. For a deterministic connectivity check use `--smoke`. If the gateway runs on a separate server, follow the manual SSH and ADB instructions below.

## Install and export the phone enrollment

On the computer physically connected to the phone, enable USB debugging, connect the USB cable, and authorize the computer on the phone. Use the repository's ADB helper on this computer. It selects the device automatically when exactly one is visible. For multiple devices, run `adb devices` and pass `--serial SERIAL` before each helper command. Python 3 validates the public server profile before the helper copies it to the phone.

```sh
./mobile/scripts/phone-adb install /path/to/app-debug.apk
```

1. Open **Harw** and allow notifications when prompted to show the connection service status.
2. In **Connection**, tap **Export enrollment**. Save the document in **Downloads** as exactly `harw-device-enrollment.json`. The app creates a phone key on first use and reuses it across app restarts and APK updates that preserve app data.
3. Pull the public document to a fresh local filename on the PC.

   ```sh
   ./mobile/scripts/phone-adb pull-enrollment ./harw-device-enrollment.json
   ```

If Documents shows a duplicate filename, keep track of which export is current. Use the internal storage **Download** folder if the Downloads shortcut omits files copied over ADB.

## Run the gateway

Run this on the computer that will host the gateway. If it is a separate server, first copy the public enrollment there with `scp` or another trusted transfer. Use regular enrollment and provider files owned by the current user or root, with write permission limited to the owner. Pick a fresh absolute test state path under an existing directory. The launcher creates it with mode 0700 and refuses to reuse it.

```sh
STATE="/tmp/harw-phone-test-$(date +%s)"
"$BOOTSTRAP" serve --smoke \
  --state "$STATE" --enrollment /path/to/harw-device-enrollment.json \
  --harw "$(command -v harw)" --auth-hub "$PWD/target/debug/harw-auth-hub"
```

Keep the gateway running. Once it reports **Public server profile**, use `$STATE/harw-server-profile.json` on the PC. For a separate server, copy that public file to the PC with `scp`. The default listener and profile endpoint use port 7443. If it is occupied, select a free port with `--port PORT` and use that port consistently for SSH and ADB below.

The smoke fixture always replies with **HARW MOBILE SMOKE REPLY**. It tests the real gateway, client authentication, streamed reply delivery, and durable transcript. The fixture provides text only. The phone has operator-level session access, including observe, submit, and interrupt. Approval requires `--allow-approval`, which explicitly sets this one enrolled device as the approver. Server-side sessions run with the observer sandbox tier. Host or gateway administration is outside the phone's grant.

### Connect a ChatGPT plan on your server

For ChatGPT sign-in, run the launcher with the model ID enabled for your account. Select this mode when starting the gateway, then import its public profile and connect the app over USB as described below.

```sh
STATE="/tmp/harw-phone-chatgpt-$(date +%s)"
"$BOOTSTRAP" serve --state "$STATE" \
  --enrollment /path/to/harw-device-enrollment.json \
  --chatgpt-model "$CHATGPT_MODEL" \
  --harw "$(command -v harw)" --auth-hub "$PWD/target/debug/harw-auth-hub"
```

This sets `session_listener.provider_link_device` to the enrolled phone. The selected phone can manage the ChatGPT account used by sessions on this gateway. The HARW device enrollment and the user's ChatGPT plan consent are both required. Treat the test state directory as private once an account is linked.

The helper writes `provider_link_device` and the `chatgpt-plan` provider only when it is started with `--chatgpt-model`, by design. A gateway started without it has no provider link.

### ChatGPT plan on a permanent gateway

Use this for a long-running gateway that was not started by the helper and whose global config already has `[session_listener]` with `enabled = true`, `tier` and `approval_device` set to the phone. The helper does not touch such a gateway, so the provider link is configured by hand.

1. Find the phone device id. It is the value already used as `approval_device`, or it appears in the harw device registry under `<state>/session-host/remote`.
2. In the global config `<harw home>/config.toml`, add `provider_link_device = "<phone device id>"` to `[session_listener]`. Keep `enabled = true`, `tier` and `approval_device`. Project configs cannot set this key.
3. Create `<harw home>/providers/chatgpt-plan.toml`. It has no auth, `api_key` or headers.

   ```toml
   name = 'chatgpt-plan'
   api = 'openai-chatgpt-plan'
   base_url = 'https://api.openai.com/v1'
   models = ['<eligible model id>']
   stream = true
   ```

4. Create `<harw home>/models/chatgpt-plan.toml`.

   ```toml
   id = '<eligible model id>'
   provider = 'chatgpt-plan'

   [capabilities]
   streaming = true
   tool_calling = true
   ```

5. Restart the gateway. The config is read only at start.
6. Check the server requirements. The harw home must be writable and owned by the server user. The credential file `<harw home>/secrets/chatgpt-plan.json` is created as a private file in a 0700 directory. The server needs outbound HTTPS to `auth.openai.com` (JWKS check) and `api.openai.com`. The link itself needs no AuthHub grant or namespace, because AuthHub only holds the node identity key for `[session_listener]`. Do not put the ChatGPT tokens in AuthHub.
7. In the app, open Connection, then ChatGPT account, then Continue with ChatGPT. If the card says the server is not set up for ChatGPT plan linking, `provider_link_device` is missing (step 2) or the gateway was not restarted. If it says this phone is not the link device, `provider_link_device` holds a different device id.
8. To actually use the plan, requests must route to the `chatgpt-plan` provider. Phone sessions cannot pick a provider, so the gateway default (`default_provider` and `default_model` in `config.toml`) must be `chatgpt-plan`. While `default_provider = "openai"`, the linked plan stays unused.

### Real model provider

Select one existing provider TOML and its corresponding model TOML. The provider's `name` must match the model's `provider`. The launcher reads those two definitions and starts a new test home. The launcher supports an existing `auth = "env:NAME"` provider reference, or explicit `auth_header = "none"` for an unauthenticated provider endpoint. Set the referenced variable in the server shell through the usual credential manager. Literal `api_key`, custom headers, and credential references that depend on the production home are outside this launcher. The selected credential reaches only the local gateway process environment.

```sh
STATE="/tmp/harw-phone-real-$(date +%s)"
"$BOOTSTRAP" serve --state "$STATE" \
  --enrollment /path/to/harw-device-enrollment.json \
  --provider-file /path/to/provider.toml --model-file /path/to/model.toml \
  --harw "$(command -v harw)" --auth-hub "$PWD/target/debug/harw-auth-hub"
```

For isolated test restarts that reuse the same server pin, add `--identity-dir /absolute/dedicated-identity-dir --kek-file /absolute/private-keys/test-authhub.key`. Supply an existing owner-only key file with exactly 32 random bytes. Keep it outside the identity and test-state directories. Give each launch a fresh `--state` path. AuthHub seals the ML-DSA key in the identity directory, and a lock prevents simultaneous use. The default in-memory identity is transient. A new launch without the sealed identity requires importing its new server pin.

## Use the pairing QR codes

To display the public gateway profile on the computer, install the Python package `segno` (Debian package `python3-segno`) and run the following command using the gateway’s test state path. Open the resulting SVG at full size on the computer and scan it in Harw. The ADB reverse or network route must still reach the endpoint in the profile.

```sh
./mobile/scripts/phone-qr profile "$STATE/harw-server-profile.json" "$STATE/server-profile.svg"
```

If the computer has a scanner that prints the phone’s enrollment QR as text, pass that text on stdin to `./mobile/scripts/phone-qr enrollment`. Its stdout is the public enrollment JSON used by the launcher. The USB file exchange above also works with a computer that has no camera.

## Connect from the phone

### Connect over your tailnet

Connect the phone and server to the same Tailscale network, then add `--tailnet` to the server’s `mobile-bootstrap serve` command. The launcher reads this server’s assigned address from the running Tailscale daemon and binds the authenticated gateway there. Its public profile includes that address and the gateway key. Import the profile or scan its QR code on the phone, then tap **Connect**. The phone must be enrolled on the gateway even if it can reach the server over Tailscale. For this mode the USB cable is only needed if you use it to install the APK or transfer the enrollment. Your tailnet policy must allow the phone to reach the selected gateway port.

### Connect over Tor (.onion)

Use this route when the phone should reach the gateway without an open port or Tailscale. The gateway publishes a Tor onion service and keeps listening on `127.0.0.1`. Build the gateway with onion support first. The first build compiles arti and takes a long time.

```sh
cargo build --release -p harw-cli --features onion --bin harw
HARW_BIN="$PWD/target/release/harw"
```

If you set `CARGO_TARGET_DIR`, use `$CARGO_TARGET_DIR/release/harw` instead. Then start the launcher with the onion flags. Keep the onion directory between runs, because it holds the onion identity and the `.onion` address stays the same only while it is kept.

```sh
STATE="/tmp/harw-phone-onion-$(date +%s)"
"$BOOTSTRAP" serve --onion --onion-dir "$HOME/.local/state/harw-phone-onion" \
  --enrollment /path/to/harw-device-enrollment.json \
  --state "$STATE" --harw "$HARW_BIN" --auth-hub "$PWD/target/debug/harw-auth-hub"
```

Wait for the gateway to print `session onion  : reachable`. Before that it prints Tor bootstrap progress lines, and the first start can take several minutes. If it prints `not confirmed reachable after 180 s; continuing`, the gateway still runs, but check the phone connection before trusting it. The launcher writes the public profile with the `.onion` locator to `$STATE/harw-server-profile.json`. Render the QR with `./mobile/scripts/phone-qr profile`, which emits a v2 code when the profile has an onion route, or import the profile file as described for USB. No USB reverse or SSH forward is needed for this route.

On the phone, open **Connection**, choose **Auto** or **Tor (.onion)** as the route, and tap **Connect**. The first Tor bootstrap on the phone can take one to two minutes and shows its progress in the app. When the link is up, the status chip shows `Live · onion`.

Keep these limits in mind:

- Tor gives no identity. The phone still needs its enrollment on the gateway and the pinned server key from the profile. A reachable `.onion` address proves nothing about who is answering.
- Tor may require a newer protocol than this build's arti 0.48. If the gateway or app exits with an outdated-software error, rebuild the gateway and the app with a newer harw.

### USB with the gateway on the same PC

Run the following on the PC while its gateway is active.

```sh
./mobile/scripts/phone-adb reverse 7443
./mobile/scripts/phone-adb push-profile /path/to/harw-server-profile.json
```

### USB with the gateway on a separate server

Keep the phone connected by USB to the PC. On the PC, open a separate terminal and keep this SSH local forward active.

```sh
ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:7443:127.0.0.1:7443 user@server
```

Back on the PC, create the phone-to-PC reverse and copy the server profile to the phone.

```sh
./mobile/scripts/phone-adb reverse 7443
./mobile/scripts/phone-adb push-profile /path/to/harw-server-profile.json
```

The phone connects to its own `127.0.0.1:7443`. ADB forwards that stream to the PC, and SSH forwards it to the remote server's loopback gateway. The server profile's endpoint and ML-DSA pin stay the same for both USB setups.

## Continue the same session on the computer

On the gateway host, open a second terminal and run `harw attach --socket "$STATE/session.sock"` with the test state path used by the launcher. The local socket and the enrolled phone use the same session host and tenant. Select a phone session on the computer, send a message, and check the reply in both windows.

## Exercise the app

1. In **Connection**, tap **Import profile**, select `Download/harw-server-profile.json`, and tap **Connect**. Verify that the app shows a connection and lists sessions.
2. Create a session, open it, and submit a short message. Confirm the message appears once and the assistant reply streams into the transcript. In smoke mode the expected reply is **HARW MOBILE SMOKE REPLY**.
3. Open a second session in a new window. Send separate messages in both sessions and verify each window retains its own transcript. Rotate the phone and use Android's split-screen option to check the windows.
4. Leave and reopen the app while the foreground notification is active. Confirm the sessions are available and any running turn was submitted only once. Disconnect from the connection screen or stop the connection from its notification, then connect again.
5. To exercise a failed pin on the phone, create a copy of the valid profile and replace only its `server_public_key_hex` with the `public_key_hex` from the phone's enrollment. Push and import this altered profile while disconnected. Connection should fail before session access. Push and import the original profile to recover.
6. To check **Interrupt**, use a turn long enough to cancel while running. To check **Approve** and **Reject**, use an authorized real-provider setup with a harmless action that parks for approval and start the launcher with `--allow-approval`. The text-only smoke fixture cannot trigger an approval.

## Stop the test

Disconnect on the phone, run `./mobile/scripts/phone-adb remove-reverse 7443` on the PC, then stop the SSH forward if one was used. Stop the bootstrap process with Ctrl+C. The test home remains at the path you selected. The same debug APK can be reinstalled with its app data intact.

This test covers the current hosted-session protocol. File replication, local-agent relay, and the later compute-plane phases require separate delivery. The host smoke test exercises the gateway and Rust client. Signing and packaging checks exercise the APK artifact. USB, Android Keystore, multiwindow, notification, and physical-phone behavior are checked on the phone.
