# tiny-dfr Ben Fork

This context names the Touch Bar concepts used in Ben's experimental `tiny-dfr` fork. It exists so agents can discuss the fork, its hardware boundary, and its Niri integration without inventing new labels each time.

## Language

**Ben fork**:
Ben's experimental fork of upstream `AsahiLinux/tiny-dfr`, developed for the Arch/T2 MacBook Touch Bar setup.
_Avoid_: patched tiny-dfr, custom tiny-dfr, our tiny-dfr

**Stock tiny-dfr**:
The distro-packaged tiny-dfr installation kept as the rollback path.
_Avoid_: original service, old tiny-dfr

**tiny-dfr-ben**:
The installed service and binary name for the Ben fork when it is live on the machine.
_Avoid_: fork service, test daemon

**Touch Bar**:
The narrow Apple display/input strip that tiny-dfr renders to and receives touches from.
_Avoid_: screen strip, mini display

**DFR**:
The dynamic function row hardware interface exposed to Linux for the Touch Bar.
_Avoid_: touchbar hardware layer, bar device

**Touch Bar DRM card**:
The DRM card that represents the Touch Bar display, distinct from the normal laptop/external display GPU card.
_Avoid_: display card, graphics card

**Workspace button**:
A Touch Bar button that asks Niri to focus a specific workspace.
_Avoid_: desktop button, space button

**Workspace highlight**:
Persistent visual state showing which workspace button corresponds to the currently focused Niri workspace.
_Avoid_: active press, selected desktop

**Control group**:
A collapsed Touch Bar button that represents a family of controls, such as brightness, backlight, volume, or media.
_Avoid_: folder, menu button, group launcher

**Overlay**:
A temporary expanded Touch Bar view for a control group.
_Avoid_: submenu, popover, modal

**Control socket**:
The narrow local state interface that external companions use to update tiny-dfr state.
_Avoid_: command socket, RPC API, shell bridge

**Niri watcher**:
The user-session companion that observes Niri workspace state and sends state updates to tiny-dfr.
_Avoid_: Niri daemon, workspace script

**Now Playing widget**:
The middle-region Touch Bar widget showing the playing track's title, artist, and album art; tapping it asks the helper to focus the player window.
_Avoid_: media widget, song display, MPRIS panel

**USB wedge**:
The appletbdrm failure where the Touch Bar's USB display channel stops responding (kernel `-110` errors); can strike with any damage traffic, not just critters.
_Avoid_: display crash, DRM hang

**Config-1 wedge**:
The post-wedge state where the Touch Bar re-enumerates in USB configuration 1 (HID input only, no display interface) or stays unconfigured; a full power-off is the first reliable recovery (a warm reboot does not clear it because the T2 keeps the hang), with an SMC reset as the next rung if a plain power-off fails. Config force-switching remains forbidden on this machine.
_Avoid_: HID mode, half-dead bar

**Rollback**:
Returning from `tiny-dfr-ben` to stock tiny-dfr while preserving a usable Touch Bar.
_Avoid_: uninstall, revert everything
