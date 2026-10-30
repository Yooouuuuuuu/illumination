// SPDX-License-Identifier: GPL-3.0-only
//! WASAPI audio (architecture §8): loopback capture of what the host plays, and playback on the
//! viewer. Both ask the audio engine for 48 kHz stereo 16-bit PCM and let it convert
//! (`AUTOCONVERTPCM`), so the rest of the pipeline sees one format. Shared mode, polled every few
//! milliseconds from the audio thread that owns the objects (COM interfaces stay on it).

use illum_media::MediaError;
use illum_media::audio::{AudioSource, CHANNELS, SAMPLE_RATE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient,
    IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM,
    WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

/// Engine buffer on both sides, in 100 ns units (100 ms).
const BUFFER_HNS: i64 = 1_000_000;

fn aerr(what: &str, e: windows::core::Error) -> MediaError {
    MediaError::Capture(format!("{what}: {e}"))
}

fn pcm_format() -> WAVEFORMATEX {
    let block_align = (CHANNELS * 2) as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

/// Opens the default render device's audio client; COM is initialised for this thread.
fn default_render_client() -> Result<IAudioClient, MediaError> {
    unsafe {
        // Already-initialised (even in another mode) is fine for what we do.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| aerr("MMDeviceEnumerator", e))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| aerr("default audio output", e))?;
        device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| aerr("IAudioClient", e))
    }
}

/// Captures what the host is playing (the render device's loopback).
pub struct LoopbackCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
}

impl LoopbackCapture {
    pub fn new() -> Result<LoopbackCapture, MediaError> {
        let client = default_render_client()?;
        let format = pcm_format();
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    BUFFER_HNS,
                    0,
                    &format,
                    None,
                )
                .map_err(|e| aerr("IAudioClient::Initialize (loopback)", e))?;
            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| aerr("IAudioCaptureClient", e))?;
            client.Start().map_err(|e| aerr("IAudioClient::Start", e))?;
            tracing::info!(target: "audio", "Capturing the default output (loopback) at {SAMPLE_RATE} Hz stereo.");
            Ok(LoopbackCapture { client, capture })
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

impl AudioSource for LoopbackCapture {
    fn read(&mut self, out: &mut Vec<i16>) -> Result<(), MediaError> {
        unsafe {
            loop {
                let frames = self
                    .capture
                    .GetNextPacketSize()
                    .map_err(|e| aerr("GetNextPacketSize", e))?;
                if frames == 0 {
                    return Ok(());
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut got = 0u32;
                let mut flags = 0u32;
                self.capture
                    .GetBuffer(&mut data, &mut got, &mut flags, None, None)
                    .map_err(|e| aerr("IAudioCaptureClient::GetBuffer", e))?;
                let values = got as usize * CHANNELS;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    out.extend(std::iter::repeat_n(0i16, values));
                } else {
                    let pcm = std::slice::from_raw_parts(data.cast::<i16>(), values);
                    out.extend_from_slice(pcm);
                }
                self.capture
                    .ReleaseBuffer(got)
                    .map_err(|e| aerr("IAudioCaptureClient::ReleaseBuffer", e))?;
            }
        }
    }
}

/// Plays interleaved 48 kHz stereo i16 on the default output.
pub struct Playback {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
}

impl Playback {
    pub fn new() -> Result<Playback, MediaError> {
        let client = default_render_client()?;
        let format = pcm_format();
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    BUFFER_HNS,
                    0,
                    &format,
                    None,
                )
                .map_err(|e| aerr("IAudioClient::Initialize (render)", e))?;
            let render: IAudioRenderClient = client
                .GetService()
                .map_err(|e| aerr("IAudioRenderClient", e))?;
            let buffer_frames = client
                .GetBufferSize()
                .map_err(|e| aerr("GetBufferSize", e))?;
            client.Start().map_err(|e| aerr("IAudioClient::Start", e))?;
            tracing::info!(target: "audio", "Playing on the default output at {SAMPLE_RATE} Hz stereo.");
            Ok(Playback {
                client,
                render,
                buffer_frames,
            })
        }
    }

    /// Frames (samples per channel) queued in the engine, waiting to be played.
    pub fn queued_frames(&self) -> u32 {
        unsafe { self.client.GetCurrentPadding().unwrap_or(0) }
    }

    /// Room left in the engine buffer, in frames.
    pub fn free_frames(&self) -> u32 {
        self.buffer_frames.saturating_sub(self.queued_frames())
    }

    /// Queues interleaved stereo i16; writes what fits and returns how many frames it took.
    pub fn write(&self, pcm: &[i16]) -> Result<usize, MediaError> {
        let frames = (pcm.len() / CHANNELS).min(self.free_frames() as usize);
        if frames == 0 {
            return Ok(0);
        }
        unsafe {
            let dst = self
                .render
                .GetBuffer(frames as u32)
                .map_err(|e| aerr("IAudioRenderClient::GetBuffer", e))?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr(), dst.cast::<i16>(), frames * CHANNELS);
            self.render
                .ReleaseBuffer(frames as u32, 0)
                .map_err(|e| aerr("IAudioRenderClient::ReleaseBuffer", e))?;
        }
        Ok(frames)
    }

    /// Queues `frames` of silence (concealment, or while the buffer primes).
    pub fn write_silence(&self, frames: u32) -> Result<(), MediaError> {
        let frames = frames.min(self.free_frames());
        if frames == 0 {
            return Ok(());
        }
        unsafe {
            let _ = self
                .render
                .GetBuffer(frames)
                .map_err(|e| aerr("IAudioRenderClient::GetBuffer", e))?;
            self.render
                .ReleaseBuffer(frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
                .map_err(|e| aerr("IAudioRenderClient::ReleaseBuffer", e))?;
        }
        Ok(())
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
