# Site deployment (harw.dev)

Architecture: `site/dist` -> MinIO bucket `harw.dev`, prefix `site/` -> loopback Nginx gateway (`127.0.0.1:3011`) -> Cloudflare Tunnel -> `https://harw.dev` and `https://www.harw.dev`.

Files here:

- `nginx.conf` - gateway config. Only GET is allowed; it maps `/x/` to `/harw.dev/site/x/index.html` on MinIO (`127.0.0.1:9000`), strips Authorization/Cookie, hides MinIO cache headers and sets a 60 s TTL, `nosniff` and a referrer policy. Missing objects and 403 become a plain 404. Paths such as `/docs` redirect (308) to `/docs/`. The unprivileged paths in it (`/opt/harw-nginx/...`, `/var/lib/harw-website/...`) are the reference host's layout; adjust `include`, `pid` and temp paths for your system.
- `public-site-policy.json` - MinIO bucket policy: anonymous `s3:GetObject` on `harw.dev/site/*` only. Bucket listing, writes and `releases/` stay private.
- `harw-website.service` - systemd user unit template. Replace `<NGINX_BIN>`, `<STATE_DIR>`, `<CONFIG_DIR>`.

## Build

```
make site-check      # builds site/dist and validates it
```

## Upload to MinIO

Configure an `mc` alias from your own credentials (do not commit them; use `mc alias set` interactively or from environment variables):

```
mc alias set local http://127.0.0.1:9000 "$MINIO_ACCESS_KEY" "$MINIO_SECRET_KEY"
mc mb --ignore-existing local/harw.dev
mc anonymous set-json deploy/site/public-site-policy.json local/harw.dev
mc cp --recursive site/dist/ local/harw.dev/site/
```

`mc cp` does not delete. Remove obsolete objects explicitly (`mc rm local/harw.dev/site/<path>`) or review with `mc diff site/dist local/harw.dev/site`.

## Gateway service

```
mkdir -p <STATE_DIR>/{body,proxy,fastcgi,uwsgi,scgi}
install -m 644 deploy/site/nginx.conf <CONFIG_DIR>/nginx.conf
# edit deploy/site/harw-website.service placeholders, then:
install -m 644 harw-website.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now harw-website.service
```

Check the config first with `<NGINX_BIN> -t -p <STATE_DIR>/ -c <CONFIG_DIR>/nginx.conf`. The reference host runs Debian's nginx 1.26.3 unpacked locally, not managed by dpkg.

## Cloudflare Tunnel

Use an existing or new tunnel (`cloudflared`) with public hostnames `harw.dev` and `www.harw.dev` pointing at `http://127.0.0.1:3011`. DNS records are proxied CNAMEs to the tunnel. Tunnel credentials live outside the repository; never commit them.

## Verification checklist

- [ ] `make site-check` passes.
- [ ] `mc ls --recursive local/harw.dev/site/` shows the 14 expected objects; `mc diff` against `site/dist` is empty.
- [ ] `curl -sI http://127.0.0.1:3011/` returns 200, `nosniff`, `Cache-Control: public, max-age=60`.
- [ ] Directory routes work: `/docs`, `/architecture`, `/de`, `/archive/overview-0-3-0` redirect (308) to the slash form and return 200.
- [ ] `curl -s -o /dev/null -w '%{http_code}' https://harw.dev/` is 200 and `https://www.harw.dev/` is 200.
- [ ] Non-GET (`curl -X PUT`) is rejected; unknown path returns 404 `Not found`.
- [ ] Private paths are not readable anonymously: bucket listing and `releases/` return 403/404 via MinIO.
- [ ] Browser check: English at `/`, German at `/de/`, mobile (320 px) layout, menu, status filter, crate search.
- [ ] `systemctl --user status harw-website.service` is active and survives a reboot.
