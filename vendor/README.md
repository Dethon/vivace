# Vendored dependencies

## librespot-connect

A copy of `connect/` from [librespot](https://github.com/librespot-org/librespot)
at commit `4a005903e078d512456fb008aaa9ba6246881bd9` (MIT, see `librespot-connect/LICENSE`), wired in through
`[patch]` in the workspace `Cargo.toml`.

Local changes, marked `vivace:` in `src/spirc.rs`, `src/state.rs` and
`src/state/context.rs`:

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

4. The placeholder context `spotify:web-api`, which librespot gives an
   ad-hoc track list, is never sent to Spotify for resolution: the request
   failed with HTTP 400. `SpircTask::handle_transfer` treats it as "no
   context" and takes the tracks from the transfer, where before the
   transfer was left unfinished and playback stopped after the current
   track. `add_autoplay_resolving_when_required` skips it, since Spotify
   offers no autoplay for a list it cannot name.

5. `ConnectConfig::start_positions` names where tracks start, by URI, when
   playback moves on to them by itself or by skipping (`handle_next`, and
   `handle_prev` when it goes back a track). Vivace fills it with where the
   user left the next episode, so the episode starts there instead of
   starting from zero and jumping once the position is known.

6. `ConnectConfig::resume_session` and `resume_playing` let a new session
   take back the playback a dropped one left. When Spotify's server closes
   the connection, the session ends and the device's state is stored as
   stopped; upstream takes a stored state over on connecting only within
   the same session, and a reconnect is a new one, so the device stayed
   idle and ignored play and pause. `handle_connection_id_update` takes it
   over when its session id is the dropped one (nothing else has played
   since), at the position where it stopped, playing or paused as asked.

7. `SpircTask::handle_transfer` takes over a state whose context has no
   uri at all like one with an empty uri, from its tracks or its current
   track; upstream refused it ("transfer context had no uri") and choosing
   the device did nothing. A state with nothing to play is refused before
   the device takes over, instead of leaving it active and stopped.

8. `ConnectState::context_to_provided_track` gives a track that comes without
   a uid, as every track of Liked Songs does, the uid Spotify's own clients
   give it: the first 20 hex digits of the SHA-1 of its uri. Upstream made one
   up at random. Spotify's own players, such as Soloist, look a transferred
   track up by its uid, and started one they could not find from the
   beginning; an empty uid instead stopped a sleeping Echo that woke for the
   transfer.

9. `SpircTask::handle_command` sends the request that takes playback to this
   device ("this computer") on a task of its own. Spotify answers it only
   after this device has handled the transfer it sends back, and awaiting it
   inside the loop kept that transfer from being handled: Spotify gave up
   after about three seconds, closed the device's connection ("Remote sent
   after having closed"), and the switch failed or limped through late.

10. `SpircTask::handle_request` replies to a transfer before sending the new
    state, as it does for every other request. Spotify fails a transfer it
    has no reply to within about three seconds and closes the connection;
    on a slow connection sending the state first took longer, so switching
    to that computer did nothing, and the sender was told 410 or 502.

11. `SpircTask::handle_cluster_update` marks the device inactive before
    sending its last state when another device has taken playback. Upstream
    sent that state as active, and Spotify sometimes took it as current: the
    device that had just taken playback stopped, and then this one went
    inactive too, leaving nothing playing. It happened every time between two
    computers whose clocks were a second apart and that reached different
    Spotify servers, and when handing over to speakers such as Music
    Assistant.

12. `SpircTask::notify` hands the cluster in the answer to its state to the
    `cluster_listener` only while the device is active. The answer to the
    last state of a device that has just lost playback could predate the
    takeover, and Vivace then showed nothing playing anywhere while a speaker
    played; the dealer pushes the cluster to an inactive device.

13. `SpircTask::handle_load` plays the tracks of a load whose context is
    `spotify:web-api`, the placeholder librespot names a plain track list
    with. Another client resuming such a list sends that uri with the
    tracks; upstream tried to resolve it, Spotify answered 400, and the
    device stayed active with nothing loaded, ignoring play.

## librespot-audio

A copy of `audio/` from the same commit, also wired in through `[patch]`.

Local change, marked `vivace:` in `src/fetch/receive.rs`: audio is requested
in pieces of at most 1 MB. Upstream sizes a request to one second of measured
throughput and holds each response in memory before writing it to disk, which
on a fast connection means a whole song, or tens of megabytes of a long
podcast episode, sitting in RAM at once.

## Updating

Bump the `rev` of the librespot dependencies in `crates/core/Cargo.toml` and
in both vendored `Cargo.toml` files, copy the new `connect/src` and
`audio/src` over the vendored ones, and reapply the changes above. Drop a
vendored crate once upstream no longer needs its changes.
