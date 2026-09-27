# Running the control plane (`harw web`)

`harw web` serves the local control plane: HTTP/1 over a Unix socket,
authorized per connection through `SO_PEERCRED` (Crypto Masterplan v2 §6.1).
It needs a HARW home in every mode (`--home` or `HARW_HOME`); the home holds
state and the approval store, never the system socket.

## Where it listens

| Mode | Command | Socket | Mode bits |
|------|---------|--------|-----------|
| Development (default) | `harw web` | `<home>/web.sock` | from the umask |
| System, self-bound | `harw web --system` | `/run/harw/infra/control.sock` | `0660` |
| System, socket-activated | `harw web --systemd-socket` | the socket systemd passes | set by the `.socket` unit |

- **Development.** `<home>/web.sock` is the dev fallback only. `--socket PATH`
  moves it.
- **`--system`.** Binds `/run/harw/infra/control.sock` (crypto drift report
  D1) and sets mode `0660` right after the bind, before the first
  connection is accepted. The group stays unchanged (the process's primary
  group) unless `--socket-group NAME|GID` names another group the process
  belongs to. `--socket PATH` names the path explicitly. There is **no**
  fallback to `$HOME`: if the parent directory (`/run/harw/infra`, created
  by `deploy/tmpfiles.d/harw.conf`) is missing, `harw web` exits with an
  error. The shipped deployment does not use this mode: `/run/harw/infra`
  is `0755 root:root` there, so only systemd creates sockets in it.
- **`--systemd-socket`.** Implies system mode and binds nothing itself. It
  requires `LISTEN_PID` to name this process and `LISTEN_FDS=1`, and the
  passed descriptor to be a Unix stream socket; anything else is a start-up
  error, never a silent fallback. `--socket` and `--socket-group` cannot be
  combined with it; the `.socket` unit sets path, mode and group:

  ```ini
  [Socket]
  ListenStream=/run/harw/infra/control.sock
  SocketUser=harw-control
  SocketGroup=harw-control-clients
  SocketMode=0660
  Service=harw-control.service
  RemoveOnStop=yes
  Accept=no
  ```

## System service

The shipped units run the socket-activated mode, like the other
infrastructure daemons (auth hub, netsec, SecurityHub):

- `deploy/systemd/harw-control.socket` listens on
  `/run/harw/infra/control.sock`, `0660 harw-control:harw-control-clients`.
- `deploy/systemd/harw-control.service` (`Requires=`/`After=` the socket,
  `Sockets=harw-control.socket`, no `[Install]` section) starts on the first
  connection:

  ```sh
  harw --home /var/lib/harw-control web --systemd-socket
  ```

- `harw-infra.target` pulls in the socket. Enable the target or the socket,
  never the service directly:

  ```sh
  sudo systemctl enable --now harw-infra.target      # or: harw-control.socket
  sudo usermod -aG harw-control-clients alice        # grant connect(2)
  ```

`harw-control-clients` (declared in `deploy/sysusers.d/harw.conf`) is empty
by default; admins add the users that may connect. Membership grants only
`connect(2)`: `harw web` authorizes every peer via `SO_PEERCRED`, so other
users reach `GET /v1/health` and get `403 unknown_peer` elsewhere unless
they hold a permission tier. The service needs no write access to
`/run/harw/infra` (`0755 root:root`, `deploy/tmpfiles.d/harw.conf`).

## Health, version, capabilities

Like the other infrastructure daemons (masterplan §38), the control plane
answers three `GET` routes before its operation routes. The JSON matches
what `harw-infra-client` (`src/info.rs`) parses:

```text
GET /v1/health        200 {"status": "ok", "service": "harw-control"}
GET /v1/version       200 {"service": "harw-control", "version": "<semver>", "protocol": 1}
GET /v1/capabilities  200 {"service": "harw-control", "protocol": 1, "operations": ["…"]}
```

- `/v1/health` needs no permission tier: any peer that can `connect(2)` to
  the socket may call it.
- `/v1/version` and `/v1/capabilities` need the lowest tier (`Observer`).
  A peer without a tier gets `403 {"reason": "unknown_peer"}`.
- `operations` lists every operation with a web route, sorted and without
  duplicates. It describes what exists; whether a peer may call an operation
  is still decided per request.
- Any method other than `GET` gets `405` with `Allow: GET`.

```sh
curl --unix-socket /run/harw/infra/control.sock http://localhost/v1/health
```
