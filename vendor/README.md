# Vendored dependencies

## librespot-connect

A copy of `connect/` from [librespot](https://github.com/librespot-org/librespot)
at commit `4a005903e078d512456fb008aaa9ba6246881bd9` (MIT, see `librespot-connect/LICENSE`), wired in through
`[patch]` in the workspace `Cargo.toml`.

Local change, marked `vivace:` in `src/spirc.rs`: `SpircTask::notify` emits
`PlayerEvent::SetQueue` on every state update. Upstream emits it only when a
context is loaded or the queue is edited, so skipping, going back and toggling
shuffle would leave the queue view showing a stale order.

To update: bump the `rev` of the librespot dependencies in
`crates/core/Cargo.toml` and in `librespot-connect/Cargo.toml`, copy the new
`connect/src` over `librespot-connect/src`, and reapply the change above.
Drop this directory once upstream emits the event itself.
