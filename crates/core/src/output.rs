//! Audio output to the system's default device.
//!
//! librespot's rodio sink opens its device once, when the player is built, and
//! never again. When that device goes away (a Remote Desktop reconnect
//! replaces "Remote Audio", headphones are unplugged) the stream stops taking
//! samples and every later write waits forever for room in its buffer: the
//! player hangs, silent, until the app restarts. This sink opens the current
//! default device again once its stream fails, whether that happened while
//! playing or while paused, and when the system picks another default device,
//! as it does when the headphones it fell back from are plugged in again.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

// About half a second of audio, as librespot's rodio sink keeps buffered.
const BUFFERED_CHUNKS: usize = 26;
// A stream that has played and then plays nothing for this long is taken for
// dead, for hosts that never report the failure.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);
// A new stream gets longer: some devices (Bluetooth) take seconds to start.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
// How often the system's default device is compared with the one playing.
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// The player's sink: rodio on the default device, reopened when it fails.
#[derive(Default)]
pub struct Output {
    stream: Option<Stream>,
    /// When the default device was last compared with the stream's.
    default_checked: Option<Instant>,
}

struct Stream {
    /// The device's name, which is all cpal tells devices apart by.
    device: String,
    sink: rodio::Sink,
    // Plays the sink's samples for as long as it is kept.
    _output: rodio::OutputStream,
    // Set once nothing written will play: by the device's error callback, or
    // when the stream stalls.
    failed: Arc<AtomicBool>,
    // Whether the device has taken any samples yet.
    started: AtomicBool,
}

impl Stream {
    fn open() -> SinkResult<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| SinkError::ConnectionRefused("no audio output device".into()))?;
        let name = device_name(&device);
        // Stereo 44.1 kHz when the device takes it, else at its own rate
        // (rodio resamples), else whatever it offers, as librespot does.
        let default_config = device.default_output_config().map_err(refused)?;
        let config = device
            .supported_output_configs()
            .map_err(refused)?
            .find(|config| config.channels() == cpal::ChannelCount::from(NUM_CHANNELS))
            .and_then(|config| {
                config
                    .try_with_sample_rate(cpal::SampleRate(SAMPLE_RATE))
                    .or_else(|| config.try_with_sample_rate(default_config.sample_rate()))
            })
            .unwrap_or(default_config);

        let failed = Arc::new(AtomicBool::new(false));
        let on_error = {
            let failed = failed.clone();
            move |error: cpal::StreamError| {
                // Some hosts report the same failure over and over.
                if !failed.swap(true, Ordering::Relaxed) {
                    log::warn!("audio output failed: {error}");
                }
            }
        };
        let builder = || {
            rodio::OutputStreamBuilder::from_device(device.clone())
                .map(|builder| builder.with_error_callback(on_error.clone()))
                .map_err(refused)
        };
        let mut output = match builder()?
            .with_config(&config.config())
            .with_sample_format(cpal::SampleFormat::I16)
            .open_stream()
        {
            Ok(output) => output,
            Err(error) => {
                log::warn!("unable to open {name} as asked, trying its own formats: {error}");
                builder()?.open_stream_or_fallback().map_err(refused)?
            }
        };
        output.log_on_drop(false);
        log::info!("playing to {name}");
        Ok(Self {
            device: name,
            sink: rodio::Sink::connect_new(output.mixer()),
            _output: output,
            failed,
            started: AtomicBool::new(false),
        })
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Waits until at most `queued` chunks are left to play, or the stream
    /// stops playing them.
    fn drain_to(&self, queued: usize) {
        let mut left = self.sink.len();
        let mut progressed = Instant::now();
        while left > queued && !self.failed() {
            thread::sleep(POLL_INTERVAL);
            let now_left = self.sink.len();
            if now_left < left {
                left = now_left;
                progressed = Instant::now();
                self.started.store(true, Ordering::Relaxed);
            } else {
                let timeout = if self.started.load(Ordering::Relaxed) {
                    STALL_TIMEOUT
                } else {
                    START_TIMEOUT
                };
                if progressed.elapsed() > timeout && !self.failed.swap(true, Ordering::Relaxed) {
                    log::warn!("audio output stopped playing");
                }
            }
        }
    }
}

fn device_name(device: &cpal::Device) -> String {
    device.name().unwrap_or_else(|_| "[unknown name]".into())
}

fn refused(error: impl std::fmt::Display) -> SinkError {
    SinkError::ConnectionRefused(error.to_string())
}

impl Output {
    /// The open stream, reopened on the current default device if the last
    /// one failed or another device became the default.
    fn stream(&mut self) -> SinkResult<&Stream> {
        if self.stream.as_ref().is_some_and(Stream::failed) || self.default_changed() {
            // Let go of the old device before opening the new one.
            self.stream = None;
        }
        if self.stream.is_none() {
            self.stream = Some(Stream::open()?);
        }
        Ok(self.stream.as_ref().expect("opened above"))
    }

    /// Whether the system has a default device other than the one playing,
    /// checked at most once a second. Without a default device, the stream
    /// stays where it is.
    fn default_changed(&mut self) -> bool {
        let Some(stream) = &self.stream else {
            return false;
        };
        if self
            .default_checked
            .is_some_and(|checked| checked.elapsed() < DEFAULT_CHECK_INTERVAL)
        {
            return false;
        }
        self.default_checked = Some(Instant::now());
        let Some(device) = cpal::default_host().default_output_device() else {
            return false;
        };
        let name = device_name(&device);
        if name == stream.device {
            return false;
        }
        log::info!("the default audio output is now {name}");
        true
    }
}

impl Sink for Output {
    fn start(&mut self) -> SinkResult<()> {
        self.stream()?.sink.play();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(stream) = &self.stream {
            stream.drain_to(0);
            stream.sink.pause();
            if stream.failed() {
                // Let go of the dead device now rather than at the next start.
                self.stream = None;
            }
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|error| SinkError::OnWrite(error.to_string()))?;
        let samples = converter.f64_to_f32(samples);
        let stream = self.stream()?;
        stream.sink.append(rodio::buffer::SamplesBuffer::new(
            cpal::ChannelCount::from(NUM_CHANNELS),
            SAMPLE_RATE,
            samples,
        ));
        // When the stream stops playing, the next write moves to the current
        // default device.
        stream.drain_to(BUFFERED_CHUNKS);
        Ok(())
    }
}
