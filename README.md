# Vivace

A Spotify Premium client for Windows and Linux that keeps memory low: about
13 MB of private memory on Windows when idle, where Spotify's own app takes
hundreds. It streams at up to 320 kbps Ogg Vorbis through
[librespot](https://github.com/librespot-org/librespot) and draws its
interface with [Slint](https://slint.dev)'s software renderer.

It covers most of what Spotify's desktop app does day to day:

- Home, search with "Show all", artists, albums, playlists and podcasts
- Your Library with Liked Songs, Your Episodes, playlists in folders, saved
  albums, followed artists and podcasts, filtered and sorted
- Playlist editing: create, rename, delete, add and remove tracks
- Podcasts that resume where you left off on any device, marked played
  when finished
- Spotify Connect: play here, control and transfer to other devices
- Queue, synced lyrics, keyboard shortcuts and media keys
- Volume normalisation with Spotify's levels, gapless playback, autoplay
- Starting at sign-in, and on Windows closing to the notification area

Lossless audio is not available: Spotify protects it with DRM that
librespot does not and will not circumvent.

## Install

Build the packages with `scripts/package.py` (below), then:

- **Windows**: unzip `vivace-<version>-windows-x64.zip` anywhere and run
  `vivace.exe`.
- **Debian, Ubuntu**: `sudo apt install ./vivace_<version>_amd64.deb`.
- **Other Linux**: unpack `vivace-<version>-linux-x64.tar.gz` and run
  `./install.sh`, which installs to `~/.local`.

The first start opens Spotify's sign-in page in the browser. The login is
kept in the system keyring when there is one.

## Build

Rust stable (see `rust-toolchain.toml`). On Linux Slint needs
`libfontconfig1-dev`, and librespot `libasound2-dev`.

    cargo run --release -p vivace

Windows builds cross-compile from Linux with
[cargo-xwin](https://github.com/rust-cross/cargo-xwin):

    cargo xwin build --release --target x86_64-pc-windows-msvc -p vivace

`scripts/package.py` builds both and writes the packages to `dist/`.

## Layout

- `crates/core`: sign-in, playback, Spotify Connect and catalogue access,
  with no user interface. `examples/cli.rs` exercises it against an account.
- `crates/app`: the Slint interface. `examples/live.rs` drives the real app
  headless and saves a screenshot per step; `examples/snapshot.rs` renders
  it with fixture data.
- `vendor`: librespot crates with small changes, listed in
  `vendor/README.md`.

## License

MIT, see `LICENSE`. Vivace uses Slint under its royalty-free license, which
asks for the "Made with Slint" credit in the About section of Settings.
Vivace is not affiliated with Spotify.
