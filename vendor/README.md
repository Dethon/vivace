# Vendored dependencies

## librespot-connect

A copy of `connect/` from [librespot](https://github.com/librespot-org/librespot)
at commit `4a005903e078d512456fb008aaa9ba6246881bd9` (MIT, see `librespot-connect/LICENSE`), wired in through
`[patch]` in the workspace `Cargo.toml`.

Local changes, marked `vivace:` in `src/spirc.rs` and `src/state.rs`:

1. `SpircTask::notify` emits `PlayerEvent::SetQueue` on every state update.
   Upstream emits it only when a context is loaded or the queue is edited, so
   skipping, going back and toggling shuffle would leave the queue view
   showing a stale order.
2. `ConnectConfig::cluster_listener` receives the cluster the server returns
   for this device's own state updates. The dealer only pushes updates caused
   by other devices, so without this there is no device list until something
   else on the account changes.

3. `SpircTask::handle_load` emits the shuffle and repeat modes it applies.
   Upstream announces them only at activation and when toggled, so a load
   that sets them left listeners showing the modes from before it.

To update: bump the `rev` of the librespot dependencies in
`crates/core/Cargo.toml` and in `librespot-connect/Cargo.toml`, copy the new
`connect/src` over `librespot-connect/src`, and reapply the changes above.
Drop this directory once upstream emits the event itself.
