#!/usr/bin/env python3
"""Sendet den FCM-Hinweis "neue Version" an das Topic `updates` (HTTP v1, Dienstkonto-Schlüssel).
Aufruf: fcm_send.py <versionCode> <versionName>. Ohne Schlüsseldatei wird still übersprungen."""
import base64, json, os, subprocess, sys, tempfile, time, urllib.parse, urllib.request

ROOT = os.environ.get("HARW_UPD_ROOT", os.path.expanduser("~/harw-updates"))
KEY = os.environ.get("HARW_FCM_KEY", os.path.join(ROOT, "fcm-service-account.json"))


def b64(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=")


def access_token(sa):
    now = int(time.time())
    head = b64(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    claim = b64(json.dumps({
        "iss": sa["client_email"], "scope": "https://www.googleapis.com/auth/firebase.messaging",
        "aud": "https://oauth2.googleapis.com/token", "iat": now, "exp": now + 3000}).encode())
    signing = head + b"." + claim
    with tempfile.NamedTemporaryFile("w", delete=False) as k:
        os.chmod(k.name, 0o600)
        k.write(sa["private_key"])
    try:
        sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", k.name], input=signing,
                             capture_output=True, check=True).stdout
    finally:
        os.unlink(k.name)
    jwt = (signing + b"." + b64(sig)).decode()
    data = urllib.parse.urlencode({"grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer", "assertion": jwt}).encode()
    with urllib.request.urlopen(urllib.request.Request("https://oauth2.googleapis.com/token", data), timeout=20) as r:
        return json.load(r)["access_token"]


def main():
    if not os.path.isfile(KEY):
        print("FCM: kein Dienstkonto-Schlüssel, übersprungen")
        return
    sa = json.load(open(KEY))
    tok = access_token(sa)
    body = {"message": {"topic": "updates", "data": {"type": "update", "versionCode": sys.argv[1], "versionName": sys.argv[2]},
                        "android": {"priority": "HIGH"}}}
    req = urllib.request.Request(
        f"https://fcm.googleapis.com/v1/projects/{sa['project_id']}/messages:send",
        json.dumps(body).encode(), {"Authorization": "Bearer " + tok, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        print("FCM gesendet:", r.read().decode()[:120])


if __name__ == "__main__":
    try:
        main()
    except Exception as e:  # Push ist optional; die Veröffentlichung darf daran nicht scheitern
        print("FCM-Fehler:", e)
