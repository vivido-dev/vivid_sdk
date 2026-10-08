//! Reverse audio channels. Network writes always run on a caller-owned media worker.
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vivid_protocol::audio_input::{self, InputPacket};
use vivid_protocol::cbor::Value;
use vivid_protocol::messages::{self, Envelope, StrictMap};
use vivid_protocol::resource::{ChannelFlow, TokenBucket};
use vivid_protocol::revision::ChannelGeneration;
use vivid_protocol::track::{TrackAddress, TrackConfiguration};
use vivid_protocol::wire::Record;

use crate::{invalid_data, invalid_input, lock};

type WriteRecord = dyn Fn(u16, u64, &[u8]) -> io::Result<u64> + Send + Sync;

/// Bits of a microphone packet ID numbered by its sender; the channel generation fills the rest.
///
/// Packet IDs must strictly increase for the track across channel generations, and each sender
/// owns exactly one generation. Putting the generation above these bits makes every later
/// generation's IDs exceed every earlier one's without any state shared between senders. At 50
/// packets per second, one generation runs out after more than two years of capture.
const GENERATION_PACKET_BITS: u32 = 32;

struct State {
    flow: ChannelFlow,
    /// Packets this sender has numbered; the low bits of its latest packet ID.
    packets: u32,
    last_sequence: u64,
    ended: bool,
    rate: TokenBucket,
    updated_at: Instant,
}

/// Presenter-side microphone sender shared by physical and virtual presenters.
///
/// A sender starts with zero credit. `try_send` drops a packet when credit is exhausted;
/// it never waits for a grant. Its transport callback must have a bounded write deadline.
pub struct AudioInputSender {
    address: TrackAddress,
    /// This generation's packet ID range starts above this value.
    packet_base: u64,
    state: Mutex<State>,
    send_order: Mutex<()>,
    write: Arc<WriteRecord>,
}

impl std::fmt::Debug for AudioInputSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioInputSender")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl AudioInputSender {
    pub fn new(
        config: &TrackConfiguration,
        generation: ChannelGeneration,
        write: impl Fn(u16, u64, &[u8]) -> io::Result<u64> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        if !audio_input::supports(config) || generation.get() == 0 {
            return Err(invalid_input("unsupported microphone configuration"));
        }
        let packet_base = generation
            .get()
            .checked_mul(1 << GENERATION_PACKET_BITS)
            .ok_or_else(|| invalid_input("channel generation exceeds microphone packet IDs"))?;
        Ok(Self {
            address: TrackAddress {
                context_id: config.context_id,
                surface_id: config.surface_id,
                track_id: config.track_id,
                channel_generation: generation,
            },
            packet_base,
            state: Mutex::new(State {
                flow: ChannelFlow::default(),
                packets: 0,
                last_sequence: 0,
                ended: false,
                rate: TokenBucket::new(50, 1),
                updated_at: Instant::now(),
            }),
            send_order: Mutex::new(()),
            write: Arc::new(write),
        })
    }

    /// Validate a receiver grant against this exact track and generation.
    pub fn grant(&self, record: &Record) -> io::Result<()> {
        if record.record_type != messages::MAX_CHANNEL_DATA
            || record.object_id != self.address.track_id
        {
            return Err(invalid_data("expected microphone flow grant"));
        }
        let envelope = messages::decode_control(&record.body)?;
        let value = Value::Map(envelope.payload);
        let map = StrictMap::new("microphone grant", &value, &[0, 1, 2, 3, 4, 5])?;
        if envelope.request_id != 0
            || map.required_u64(0)? != self.address.context_id
            || map.required_u64(1)? != self.address.surface_id
            || map.required_u64(2)? != self.address.track_id
            || map.required_u64(3)? != self.address.channel_generation.get()
        {
            return Err(invalid_data(
                "microphone grant has a stale owner or generation",
            ));
        }
        let bytes = map.required_u64(4)?;
        let records = map.required_u64(5)?;
        let mut state = lock(&self.state, "microphone sender")?;
        if bytes.saturating_sub(state.flow.sent_body_bytes)
            > u64::from(audio_input::BODY_BYTES) * audio_input::QUEUE_PACKETS as u64
            || records.saturating_sub(state.flow.sent_media_records)
                > audio_input::QUEUE_PACKETS as u64
        {
            return Err(invalid_data("microphone grant exceeds the bounded window"));
        }
        state.flow.raise_maxima(bytes, records);
        Ok(())
    }

    /// Re-originate PCM with independent media identity. False means dropped for lack of credit.
    pub fn try_send(&self, packet: &InputPacket) -> io::Result<bool> {
        let _order = lock(&self.send_order, "microphone send order")?;
        let id = {
            let mut state = lock(&self.state, "microphone sender")?;
            if state.ended {
                return Ok(false);
            }
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(state.updated_at);
            state.rate.replenish(elapsed).map_err(io::Error::other)?;
            state.updated_at = now;
            if state
                .rate
                .time_until(1)
                .map_err(io::Error::other)?
                .is_some()
            {
                return Ok(false);
            }
            if state.flow.admit(audio_input::BODY_BYTES).is_err() {
                return Ok(false);
            }
            let packets = state
                .packets
                .checked_add(1)
                .ok_or_else(|| invalid_data("microphone media identity exhausted"))?;
            state.rate.charge(1).map_err(io::Error::other)?;
            state.packets = packets;
            // The base has zero low bits, so this cannot carry into the generation.
            self.packet_base | u64::from(packets)
        };
        let body = InputPacket {
            epoch: 1,
            packet_id: id,
            pts_us: packet.pts_us,
            pcm: packet.pcm,
        }
        .encode()?;
        let sequence = (self.write)(messages::AUDIO_PACKET, self.address.track_id, &body)?;
        lock(&self.state, "microphone sender")?.last_sequence = sequence;
        Ok(true)
    }

    /// Close submission without charging flow. Device lifetime is independent of this channel.
    pub fn eos(&self) -> io::Result<()> {
        let _order = lock(&self.send_order, "microphone send order")?;
        let last = {
            let mut state = lock(&self.state, "microphone sender")?;
            if state.ended {
                return Ok(());
            }
            state.ended = true;
            state.last_sequence
        };
        let body = Envelope::new(
            0,
            vec![
                (0, Value::Unsigned(self.address.context_id)),
                (1, Value::Unsigned(self.address.surface_id)),
                (2, Value::Unsigned(self.address.track_id)),
                (3, Value::Unsigned(self.address.channel_generation.get())),
                (4, Value::Unsigned(if last == 0 { 0 } else { 1 })),
                (5, Value::Unsigned(last)),
            ],
        )
        .encode()?;
        (self.write)(messages::CHANNEL_EOS, self.address.track_id, &body)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use vivid_protocol::track::max_channel_data_payload;

    fn grant(sender: &AudioInputSender, records: u64) {
        let body = Envelope::new(
            0,
            max_channel_data_payload(
                sender.address,
                u64::from(audio_input::BODY_BYTES) * records,
                records,
            ),
        )
        .encode()
        .unwrap();
        sender
            .grant(&Record {
                record_type: messages::MAX_CHANNEL_DATA,
                flags: 0,
                object_id: sender.address.track_id,
                sequence: 1,
                body,
            })
            .unwrap();
    }

    /// A sender that records the packet ID of every `AUDIO_PACKET` it writes.
    fn recording_sender(
        context_id: u64,
        generation: ChannelGeneration,
    ) -> (AudioInputSender, Arc<Mutex<Vec<u64>>>) {
        let ids = Arc::new(Mutex::new(Vec::new()));
        let written = ids.clone();
        let config = audio_input::configuration(context_id, 2, 3);
        let sender = AudioInputSender::new(&config, generation, move |kind, _, body| {
            if kind == messages::AUDIO_PACKET {
                written
                    .lock()
                    .unwrap()
                    .push(InputPacket::decode(body)?.packet_id);
            }
            Ok(1)
        })
        .unwrap();
        (sender, ids)
    }

    fn packet(pts_us: i64) -> InputPacket {
        InputPacket {
            epoch: 1,
            packet_id: 1,
            pts_us,
            pcm: [0; audio_input::PCM_BYTES],
        }
    }

    #[test]
    fn packet_ids_belong_to_their_sender_and_rise_across_generations() {
        // Two owners reuse the same surface and track IDs. Each numbers its own packets, so one
        // sending does not move the other's IDs.
        let (first, first_ids) = recording_sender(1, ChannelGeneration::ONE);
        let (other, other_ids) = recording_sender(9, ChannelGeneration::ONE);
        grant(&first, 2);
        grant(&other, 1);
        assert!(first.try_send(&packet(0)).unwrap());
        // The sender paces itself to one 20 ms packet per token.
        std::thread::sleep(std::time::Duration::from_millis(25));
        assert!(first.try_send(&packet(20_000)).unwrap());
        assert!(other.try_send(&packet(0)).unwrap());
        let base = 1 << GENERATION_PACKET_BITS;
        assert_eq!(*first_ids.lock().unwrap(), [base + 1, base + 2]);
        assert_eq!(*other_ids.lock().unwrap(), [base + 1]);

        // The track's next generation starts above everything the previous one could send.
        let (next, next_ids) = recording_sender(1, ChannelGeneration::new(2));
        grant(&next, 1);
        assert!(next.try_send(&packet(40_000)).unwrap());
        let next_id = next_ids.lock().unwrap()[0];
        assert!(next_id > base + u64::from(u32::MAX));
        assert_eq!(next_id, 2 * base + 1);
    }

    #[test]
    fn a_generation_beyond_the_packet_id_space_is_refused() {
        let config = audio_input::configuration(1, 2, 3);
        let generation = ChannelGeneration::new(1 << (64 - GENERATION_PACKET_BITS));
        assert!(AudioInputSender::new(&config, generation, |_, _, _| Ok(1)).is_err());
    }

    #[test]
    fn microphone_never_sends_without_credit_and_checks_grant_identity() {
        let config = audio_input::configuration(1, 2, 3);
        let writes = Arc::new(AtomicU64::new(0));
        let counter = writes.clone();
        let sender = AudioInputSender::new(&config, ChannelGeneration::ONE, move |_, _, _| {
            Ok(counter.fetch_add(1, Ordering::Relaxed) + 1)
        })
        .unwrap();
        let packet = InputPacket {
            epoch: 1,
            packet_id: 1,
            pts_us: 0,
            pcm: [0; audio_input::PCM_BYTES],
        };
        assert!(!sender.try_send(&packet).unwrap());
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        let mut grant = Record {
            record_type: messages::MAX_CHANNEL_DATA,
            flags: 0,
            object_id: 3,
            sequence: 1,
            body: Envelope::new(
                0,
                max_channel_data_payload(sender.address, u64::from(audio_input::BODY_BYTES), 1),
            )
            .encode()
            .unwrap(),
        };
        grant.object_id = 4;
        assert!(sender.grant(&grant).is_err());
        grant.object_id = 3;
        sender.grant(&grant).unwrap();
        assert!(sender.try_send(&packet).unwrap());
        assert!(!sender.try_send(&packet).unwrap());
        sender.eos().unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 2);
        assert!(!sender.try_send(&packet).unwrap());
    }
}
