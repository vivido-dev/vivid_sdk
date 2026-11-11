//! Actual decoded timing, conversion delay and discontinuity regression checks.

// Rust guideline compliant 2026-10-07
use vivid_audio::{DecodedBatch, Decoder};
use vivid_protocol::{media::ParsedAudioPacket, track::AudioConfiguration};

fn config(rate: u32) -> AudioConfiguration {
    AudioConfiguration {
        codec: "pcm_s16le".into(),
        packetization: "pcm-packet-v1".into(),
        extradata: vec![],
        sample_rate: rate,
        channels: 2,
        channel_mask: 3,
        maximum_access_unit_bytes: 4096,
        codec_string: None,
    }
}
fn packet(data: &[u8], pts: i64) -> ParsedAudioPacket<'_> {
    ParsedAudioPacket {
        epoch: 0,
        packet_id: 1,
        pts_us: pts,
        dts_us: pts,
        // Deliberately unrelated to native sample count: timing must come
        // from decoded samples, rather than the packet's declared duration.
        duration_us: 1,
        trim_start_samples: 0,
        trim_end_samples: 0,
        data,
    }
}
fn tone(frames: usize, amplitude: i16) -> Vec<u8> {
    amplitude.to_le_bytes().repeat(frames * 2)
}
fn covered(batch: &DecodedBatch) {
    let mut cursor = 0;
    for span in batch.spans() {
        assert_eq!(span.sample_range().start, cursor);
        cursor = span.sample_range().end;
        assert_eq!(span.sample_range().len(), span.frames() as usize * 2);
    }
    assert_eq!(cursor, batch.samples().len());
}

#[test]
fn trim_moves_the_first_sample_without_moving_the_conversion_origin() {
    let mut decoder = Decoder::new(&config(48_000), 48_000, 2).unwrap();
    let data = tone(20, 4096);
    let batch = decoder
        .push_timed(ParsedAudioPacket {
            trim_start_samples: 2,
            trim_end_samples: 1,
            ..packet(&data, 1_000)
        })
        .unwrap();
    covered(&batch);
    assert_eq!((batch.sample_rate(), batch.channels()), (48_000, 2));
    let span = batch.spans()[0];
    assert_eq!(span.origin_pts_us(), Some(1_000));
    assert_eq!(span.frame_offset(), 2);
    assert_eq!(span.frames(), 17);
    assert_eq!(span.pts_us(), Some(1_041));
    assert_eq!(span.end_pts_us(), Some(1_395));
    drop(decoder);
    assert_eq!(batch.into_samples().len(), 34);
}

#[test]
fn resampler_tail_retains_the_old_timeline_before_a_gap_or_overlap() {
    for next_pts in [30_000, 5_000] {
        let mut decoder = Decoder::new(&config(24_000), 48_000, 2).unwrap();
        let a = decoder.push_timed(packet(&tone(240, 4096), 0)).unwrap();
        let b = decoder
            .push_timed(packet(&tone(240, 8192), next_pts))
            .unwrap();
        let c = decoder.finish_timed().unwrap();
        assert!(b.spans().len() >= 2, "old filter tail must precede new PCM");
        assert_eq!(b.spans()[0].timeline(), a.spans()[0].timeline());
        assert_ne!(b.spans()[0].timeline(), b.spans()[1].timeline());
        let mut frames = [0, 0];
        for batch in [&a, &b, &c] {
            covered(batch);
            for span in batch.spans() {
                let run = usize::from(span.origin_pts_us() != Some(0));
                assert_eq!(span.frame_offset(), frames[run]);
                frames[run] += span.frames();
                let expected = if run == 0 { 0.125 } else { 0.25 };
                assert!(
                    batch.samples()[span.sample_range()]
                        .iter()
                        .all(|s| (s - expected).abs() < 0.001)
                );
            }
        }
        assert_eq!(frames, [480, 480]);
        assert_eq!(
            c.spans().last().unwrap().end_pts_us(),
            Some(next_pts + 10_000)
        );
    }
}

#[test]
fn unknown_output_is_not_retroactively_retagged_when_an_anchor_arrives() {
    let mut decoder = Decoder::new(&config(24_000), 48_000, 2).unwrap();
    let a = decoder
        .push_timed(packet(&tone(240, 4096), i64::MIN))
        .unwrap();
    let b = decoder
        .push_timed(packet(&tone(240, 4096), 40_000))
        .unwrap();
    assert_eq!(a.spans()[0].pts_us(), None);
    assert_eq!(b.spans()[0].pts_us(), None);
    assert_eq!(b.spans()[1].pts_us(), Some(40_000));
    covered(&b);
}

#[test]
fn fractional_packet_durations_do_not_accumulate_timestamp_rounding() {
    let mut decoder = Decoder::new(&config(44_100), 48_000, 2).unwrap();
    let mut frames = 0;
    let mut serial = None;
    for index in 0..100 {
        let batch = decoder
            .push_timed(packet(&tone(127, 4096), index * 127 * 1_000_000 / 44_100))
            .unwrap();
        covered(&batch);
        for span in batch.spans() {
            assert_eq!(*serial.get_or_insert(span.timeline()), span.timeline());
            assert_eq!(span.origin_pts_us(), Some(0));
            assert_eq!(span.frame_offset(), frames);
            frames += span.frames();
        }
    }
    let tail = decoder.finish_timed().unwrap();
    covered(&tail);
    assert_eq!(tail.spans()[0].frame_offset(), frames);
    frames += tail.spans().iter().map(|s| s.frames()).sum::<u64>();
    assert_eq!(frames, (12_700_u64 * 48_000).div_ceil(44_100));
}

#[test]
fn timestamp_overflow_retires_the_decoder() {
    let mut decoder = Decoder::new(&config(48_000), 48_000, 2).unwrap();
    assert!(
        decoder
            .push_timed(packet(&tone(480, 4096), i64::MAX - 1))
            .is_err()
    );
    assert!(decoder.finish_timed().is_err());
}

#[test]
fn resampled_packet_trim_preserves_old_tail_and_removes_actual_final_padding() {
    let mut decoder = Decoder::new(&config(24_000), 48_000, 2).unwrap();
    let a = decoder.push_timed(packet(&tone(240, 4096), 0)).unwrap();
    let b = decoder
        .push_timed(ParsedAudioPacket {
            trim_start_samples: 2,
            trim_end_samples: 1,
            ..packet(&tone(240, 8192), 10_000)
        })
        .unwrap();
    let c = decoder.finish_timed().unwrap();
    covered(&a);
    covered(&b);
    assert!(c.samples().is_empty());
    let old_frames = a
        .spans()
        .iter()
        .chain(b.spans())
        .filter(|s| s.origin_pts_us() == Some(0))
        .map(|s| s.frames())
        .sum::<u64>();
    assert_eq!(
        old_frames, 480,
        "new trim removed the previous packet's tail"
    );
    let current = b
        .spans()
        .iter()
        .filter(|s| s.origin_pts_us() == Some(10_000))
        .collect::<Vec<_>>();
    assert_eq!(current.first().unwrap().frame_offset(), 4);
    assert_eq!(current.iter().map(|s| s.frames()).sum::<u64>(), 474);
    assert_eq!(current.last().unwrap().end_pts_us(), Some(19_958));
}
