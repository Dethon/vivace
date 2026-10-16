//! Checks that audio carries on after the output device goes away and a new
//! one takes its place, as a Remote Desktop reconnect replaces "Remote Audio".
//! Plays silence; run it through `scripts/device-loss.sh`, which gives it a
//! private PulseAudio server and sets `DEVICE_LOSS_REPLACE` to a command that
//! restarts that server.
//!
//!   device_loss paused     the device goes while paused, then playback resumes
//!   device_loss playing    the device goes mid-play; the write fails, the
//!                          player pauses, and play resumes on the new device
//!
//! Without `DEVICE_LOSS_REPLACE` (as on Windows) the device stays, which still
//! checks that the sink plays in real time.

use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use librespot_playback::{audio_backend::Sink, convert::Converter, decoder::AudioPacket};
use vivace_core::Output;

const DEADLINE: Duration = Duration::from_secs(10);

// Writes like the player: when a write fails it stops the sink (pauses), and
// the next play starts it again.
fn silence(sink: &mut Output, seconds: f64) {
    let mut converter = Converter::new(None);
    // About 1600 stereo samples per packet, like the decoder.
    let packets = (seconds * 44_100.0 * 2.0 / 1600.0) as usize;
    for _ in 0..packets {
        if let Err(error) = sink.write(AudioPacket::Samples(vec![0.0; 1600]), &mut converter) {
            eprintln!("write failed, pausing: {error}");
            sink.stop().expect("stop");
            std::thread::sleep(Duration::from_secs(1));
            sink.start().expect("start after the new device came");
        }
    }
}

fn replace_device() {
    let Ok(command) = std::env::var("DEVICE_LOSS_REPLACE") else {
        return;
    };
    Command::new("sh")
        .args(["-c", &command])
        .status()
        .expect("replace the device");
    eprintln!("replaced the output device");
}

fn main() {
    let scenario = std::env::args().nth(1).unwrap_or_default();
    let paused = match scenario.as_str() {
        "paused" => true,
        "playing" => false,
        _ => {
            eprintln!("usage: device_loss paused|playing");
            std::process::exit(2);
        }
    };
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = Output::default();
        sink.start().expect("start");
        silence(&mut sink, 1.0);
        let started = Instant::now();
        if paused {
            sink.stop().expect("stop");
            replace_device();
            sink.start().expect("start");
            silence(&mut sink, 2.0);
        } else {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                replace_device();
            });
            silence(&mut sink, 3.0);
        }
        sink.stop().expect("stop");
        _ = done_tx.send(started.elapsed());
    });
    match done.recv_timeout(DEADLINE) {
        Ok(took) => println!("PASS ({scenario}): audio carried on, in {took:?}"),
        Err(_) => {
            println!("FAIL ({scenario}): playback hung after the device went away");
            std::process::exit(1);
        }
    }
}
