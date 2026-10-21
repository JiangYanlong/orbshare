use ruopus::{OpusEncoder, Signal};

pub const OPUS_SAMPLE_RATE: u32 = 48_000;
pub const OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 960;
const OPUS_MAX_PACKET_BYTES: usize = 1_275;
const SYSTEM_AUDIO_BITRATE: u32 = 96_000;

/// Stateful 20 ms Opus packetizer for ScreenCaptureKit PCM.
///
/// `ruopus` is a pure-Rust codec compiled into OrbShare, so this path does not
/// start FFmpeg, link libopus, or require a separately installed runtime.
pub struct SystemAudioOpusEncoder {
    encoder: OpusEncoder,
    channels: usize,
    pending: Vec<f32>,
}

impl SystemAudioOpusEncoder {
    pub fn new(channels: u16) -> Result<Self, String> {
        if !(1..=2).contains(&channels) {
            return Err(format!("Opus 仅支持 1 或 2 声道，收到 {channels} 声道"));
        }
        let mut encoder = OpusEncoder::new(channels as usize);
        encoder.set_bitrate(Some(SYSTEM_AUDIO_BITRATE));
        encoder.set_signal(Signal::Music);
        Ok(Self {
            encoder,
            channels: channels as usize,
            pending: Vec::with_capacity(OPUS_FRAME_SAMPLES_PER_CHANNEL * channels as usize * 2),
        })
    }

    /// Appends interleaved f32 PCM and returns every complete 20 ms Opus packet.
    pub fn push_pcm(
        &mut self,
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
    ) -> Result<Vec<Vec<u8>>, String> {
        if sample_rate != OPUS_SAMPLE_RATE {
            return Err(format!(
                "系统声音采样率必须为 {OPUS_SAMPLE_RATE} Hz，收到 {sample_rate} Hz"
            ));
        }
        if channels as usize != self.channels {
            return Err(format!(
                "系统声音声道数发生变化：编码器 {} 声道，收到 {channels} 声道",
                self.channels
            ));
        }
        if samples.len() % self.channels != 0 {
            return Err("系统声音 PCM 不是完整的交错声道帧".to_string());
        }

        self.pending.extend_from_slice(samples);
        let frame_len = OPUS_FRAME_SAMPLES_PER_CHANNEL * self.channels;
        let mut packets = Vec::new();
        while self.pending.len() >= frame_len {
            let remainder = self.pending.split_off(frame_len);
            let frame = std::mem::replace(&mut self.pending, remainder);
            let packet = self
                .encoder
                .encode_auto(&frame, OPUS_MAX_PACKET_BYTES)
                .map_err(|error| format!("系统声音 Opus 编码失败：{error:?}"))?;
            if packet.is_empty() {
                return Err("系统声音 Opus 编码返回了空包".to_string());
            }
            packets.push(packet);
        }
        Ok(packets)
    }

    #[cfg(test)]
    fn buffered_samples(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_one_stereo_twenty_millisecond_packet() {
        let mut encoder = SystemAudioOpusEncoder::new(2).expect("create encoder");
        let packets = encoder
            .push_pcm(&vec![0.0; OPUS_FRAME_SAMPLES_PER_CHANNEL * 2], 48_000, 2)
            .expect("encode silence");

        assert_eq!(packets.len(), 1);
        assert!(!packets[0].is_empty());
        assert!(packets[0].len() <= OPUS_MAX_PACKET_BYTES);
        let decoded = ruopus::OpusDecoder::new(2)
            .decode_packet(&packets[0])
            .expect("decode generated Opus");
        assert_eq!(decoded.len(), OPUS_FRAME_SAMPLES_PER_CHANNEL * 2);
        assert!(decoded.iter().all(|sample| sample.is_finite()));
        assert_eq!(encoder.buffered_samples(), 0);
    }

    #[test]
    fn buffers_partial_pcm_until_a_complete_opus_frame_exists() {
        let mut encoder = SystemAudioOpusEncoder::new(2).expect("create encoder");
        let half = vec![0.0; OPUS_FRAME_SAMPLES_PER_CHANNEL];

        assert!(encoder.push_pcm(&half, 48_000, 2).unwrap().is_empty());
        assert_eq!(encoder.buffered_samples(), OPUS_FRAME_SAMPLES_PER_CHANNEL);
        assert_eq!(encoder.push_pcm(&half, 48_000, 2).unwrap().len(), 1);
    }

    #[test]
    fn rejects_an_unexpected_capture_format() {
        let mut encoder = SystemAudioOpusEncoder::new(2).expect("create encoder");
        assert!(encoder.push_pcm(&[0.0, 0.0], 44_100, 2).is_err());
        assert!(encoder.push_pcm(&[0.0, 0.0], 48_000, 1).is_err());
    }
}
