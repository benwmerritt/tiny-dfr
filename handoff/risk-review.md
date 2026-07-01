# T2/systemd/socket safety review

Subagent run: `/home/ben/.pi/agent/subpi-runs/20260701101353-290051/output.log`

## Files inspected

- `AGENTS.md`
- `FORK.md`
- `etc/systemd/system/tiny-dfr.service`
- `etc/systemd/system/systemd-backlight@backlight:*.service`
- `etc/udev/rules.d/99-touchbar-seat.rules`
- `etc/udev/rules.d/99-touchbar-tiny-dfr.rules`
- `/home/ben/archdots/.config/tiny-dfr/README.md`
- `/home/ben/archdots/.config/tiny-dfr/config.toml`
- `/home/ben/archdots/scripts/install-tiny-dfr-system-links.sh`
- approved Markdown plan
- relevant `src/main.rs` and `src/config.rs` paths

No files were changed by the reviewer.

## Blockers before live install

### Do not install repo udev rule as-is

`etc/udev/rules.d/99-touchbar-tiny-dfr.rules` contains:

```udev
ATTR{bConfigurationValue}="0", ATTR{bConfigurationValue}="2"
```

This violates the machine safety rule: do not force-switch `05ac:8302`; it can wedge the Touch Bar until rebind/reboot.

### Do not run stock and fork services together

`tiny-dfr.service` and any future `tiny-dfr-ben.service` will contend for the same Touch Bar DRM/input/uinput ownership.

### Avoid hard dependency on `dev-tiny_dfr_display.device`

The packaged service expects a systemd alias that does not appear on Ben's Intel/T2 machine. Any `tiny-dfr-ben.service` should copy the archdots workaround and avoid a hard `BindsTo=dev-tiny_dfr_display.device` dependency.

## Safety constraints to preserve

- Keep `ProtectHome=true`.
- Keep Niri watcher as a user-session process.
- No shell-command execution in tiny-dfr.
- No runtime config rewrite loop for workspace active state.
- No service restart/install/udev trigger/sudo without explicit approval.
- Keep stock `/usr/bin/tiny-dfr`, stock package, and last-known-good config recoverable.

## Socket permission recommendation

- Path: `/run/tiny-dfr-ben/control.sock`.
- Systemd settings:
  - `RuntimeDirectory=tiny-dfr-ben`
  - `RuntimeDirectoryMode=0750`
  - keep `RestrictAddressFamilies=AF_UNIX AF_NETLINK`
  - keep `PrivateTmp=true`, `PrivateIPC=true`, `ProtectHome=true`
- Permissions:
  - dedicated group such as `tiny-dfr-control`
  - socket owner `root:tiny-dfr-control`
  - socket mode `0660`
  - only Ben / watcher user belongs to that group
- Avoid broad groups like `input` or `video` for socket write permission.

## Protocol constraints

Socket protocol must be state-only:

- JSON or similarly narrow messages only.
- Reject unknown fields/message types.
- Size-limit messages.
- Fail closed on parse errors.
- No file paths, commands, device operations, service operations, or arbitrary key/action dispatch.

## Live-test gate

Before any live test:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build
```

Also confirm:

1. separate binary path, e.g. `/usr/bin/tiny-dfr-ben`;
2. separate service name: `tiny-dfr-ben.service`;
3. stock `tiny-dfr.service` can restart successfully;
4. terminal open with rollback commands ready;
5. explicit Ben approval in the current chat.

## Rollback checklist

```sh
sudo systemctl stop tiny-dfr-ben.service
sudo systemctl disable tiny-dfr-ben.service
sudo systemctl reset-failed tiny-dfr-ben.service

sudo systemctl restart tiny-dfr.service
systemctl --no-pager --lines=50 status tiny-dfr.service
```

Keep ready:

- known-good `/etc/tiny-dfr/config.toml`;
- archdots installer: `~/archdots/scripts/install-tiny-dfr-system-links.sh`;
- no USB config switching unless Ben explicitly decides otherwise.

## Things not to do

- Do not install `99-touchbar-tiny-dfr.rules` unchanged.
- Do not force-switch `05ac:8302`.
- Do not overwrite stock `tiny-dfr.service` for the experimental fork.
- Do not weaken `ProtectHome=true`.
- Do not expose socket to world/group `input`.
- Do not let socket trigger key events, shell commands, config writes, or device operations.
- Do not rely on `/dev/tiny_dfr_display` for Ben's T2 service startup.
