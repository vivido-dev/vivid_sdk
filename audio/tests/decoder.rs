//! Canonical codec, resampling, resource and lifetime checks without devices.

// Rust guideline compliant 2026-10-07
use vivid_audio::{CaptureResampler, Decoder, Limits};
use vivid_protocol::{media::ParsedAudioPacket, track::AudioConfiguration};

fn pcm() -> AudioConfiguration {
    AudioConfiguration {
        codec: "pcm_s16le".into(),
        packetization: "pcm-packet-v1".into(),
        extradata: vec![],
        sample_rate: 48_000,
        channels: 2,
        channel_mask: 3,
        maximum_access_unit_bytes: 4096,
        codec_string: None,
    }
}

fn packet(data: &[u8]) -> ParsedAudioPacket<'_> {
    ParsedAudioPacket {
        epoch: 0,
        packet_id: 1,
        pts_us: 0,
        dts_us: 0,
        duration_us: 10_000,
        trim_start_samples: 0,
        trim_end_samples: 0,
        data,
    }
}

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn compressed_fixture(text: &str) {
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    let config = AudioConfiguration {
        codec: v["codec"].as_str().unwrap().into(),
        packetization: v["packetization"].as_str().unwrap().into(),
        extradata: unhex(v["extradata"].as_str().unwrap()),
        ..pcm()
    };
    let mut decoder = Decoder::new(&config, 48_000, 2).unwrap();
    let mut output = vec![];
    let mut first_pts = None;
    for (id, p) in v["packets"].as_array().unwrap().iter().enumerate() {
        let data = unhex(p["data"].as_str().unwrap());
        let batch = decoder
            .push_timed(ParsedAudioPacket {
                packet_id: id as u64 + 1,
                pts_us: p["pts_us"].as_i64().unwrap(),
                dts_us: p["pts_us"].as_i64().unwrap(),
                duration_us: p["duration_us"].as_u64().unwrap(),
                trim_start_samples: p["trim_start"].as_u64().unwrap() as u32,
                trim_end_samples: p["trim_end"].as_u64().unwrap() as u32,
                ..packet(&data)
            })
            .unwrap();
        let mut through = 0;
        for span in batch.spans() {
            assert_eq!(span.sample_range().start, through);
            through = span.sample_range().end;
            first_pts.get_or_insert(span.pts_us().expect("fixture frame must have a PTS"));
        }
        assert_eq!(through, batch.samples().len());
        output.extend(batch.into_samples());
    }
    output.extend(decoder.finish().unwrap());
    assert_eq!(
        first_pts,
        Some(0),
        "priming or pre-skip retagged the first audible sample"
    );
    assert_eq!(
        output.len() as u64,
        v["reference_samples"].as_u64().unwrap(),
        "{} sample count differs from independent CLI decode",
        config.codec
    );
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(
        output.iter().any(|sample| sample.abs() > 0.001),
        "fixture tone was lost"
    );
}

#[test]
fn opus_pre_skip_is_applied_once_and_final_padding_is_trimmed() {
    compressed_fixture(include_str!("../fixtures/opus.json"));
}

#[test]
fn vorbis_canonical_headers_and_delayed_output_match_cli_decode() {
    compressed_fixture(include_str!("../fixtures/vorbis.json"));
}

#[test]
fn flac_streaminfo_and_frames_match_cli_decode() {
    compressed_fixture(include_str!("../fixtures/flac.json"));
}

#[test]
fn aac_canonical_initialization_and_priming_match_cli_decode() {
    compressed_fixture(include_str!("../fixtures/aac.json"));
}

#[test]
fn resampling_retains_the_eos_filter_tail_and_channel_alignment() {
    let mut config = pcm();
    config.sample_rate = 24_000;
    let mut decoder = Decoder::new(&config, 48_000, 2).unwrap();
    let data: Vec<_> = (0..480).flat_map(|_| 4096_i16.to_le_bytes()).collect();
    let mut samples = decoder.push(packet(&data)).unwrap();
    let before = samples.len();
    samples.extend(decoder.finish().unwrap());
    assert_eq!(samples.len(), 960);
    assert!(before < samples.len(), "EOS omitted the resampler tail");
    assert!(samples.iter().all(|sample| (*sample - 0.125).abs() < 0.001));
    assert!(decoder.finish().is_err());
    assert!(decoder.push(packet(&data)).is_err());
}

#[test]
fn samples_survive_decoder_retirement_and_another_decoder_failure() {
    let data = vec![0; 1920];
    let mut healthy = Decoder::new(&pcm(), 48_000, 2).unwrap();
    let retained = healthy.push(packet(&data)).unwrap();
    let mut failed = Decoder::with_limits(
        &pcm(),
        48_000,
        2,
        Limits {
            max_output_samples: 32,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(failed.push(packet(&data)).is_err());
    assert!(failed.push(packet(&[])).is_err());
    assert_eq!(healthy.push(packet(&data)).unwrap().len(), 960);
    drop(healthy);
    assert_eq!(retained, vec![0.; 960]);
}

#[test]
fn packet_and_trim_limits_fail_before_native_work() {
    let oversized = vec![0; 4097];
    let mut d = Decoder::new(&pcm(), 48_000, 2).unwrap();
    assert!(d.push(packet(&oversized)).is_err());
    let data = [0; 4];
    let mut d = Decoder::new(&pcm(), 48_000, 2).unwrap();
    assert!(
        d.push(ParsedAudioPacket {
            trim_start_samples: u32::MAX,
            ..packet(&data)
        })
        .is_err()
    );
    let mut d = Decoder::new(&pcm(), 48_000, 2).unwrap();
    assert!(
        d.push(ParsedAudioPacket {
            duration_us: u64::MAX,
            ..packet(&data)
        })
        .is_err()
    );
}

#[test]
fn resampler_expansion_cannot_exceed_the_converted_sample_budget() {
    let config = AudioConfiguration {
        sample_rate: 8_000,
        ..pcm()
    };
    let mut decoder = Decoder::with_limits(
        &config,
        384_000,
        2,
        Limits {
            max_output_samples: 256,
            ..Default::default()
        },
    )
    .unwrap();
    // Only 32 native samples: the conversion, rather than native frame size,
    // exceeds the admitted budget at this output rate.
    let error = decoder.push(packet(&[0; 64])).unwrap_err();
    assert_eq!(error.to_string(), "audio output exceeds sample budget");
    assert!(decoder.finish().is_err());
}

#[test]
fn a_packet_cannot_exhaust_frame_or_aggregate_batch_limits() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/aac.json")).unwrap();
    let config = AudioConfiguration {
        codec: "aac".into(),
        packetization: "aac-raw-au-v1".into(),
        extradata: unhex(fixture["extradata"].as_str().unwrap()),
        ..pcm()
    };
    // An untrusted record concatenates access units to elicit multiple native
    // frames. Both total work and converted samples must remain bounded.
    let data: Vec<u8> = fixture["packets"].as_array().unwrap()[..3]
        .iter()
        .flat_map(|p| unhex(p["data"].as_str().unwrap()))
        .collect();
    let mut work = Decoder::with_limits(
        &config,
        48_000,
        2,
        Limits {
            max_output_frames: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        work.push(packet(&data)).unwrap_err().to_string(),
        "audio decode work exceeds frame budget"
    );
    let mut batch = Decoder::with_limits(
        &config,
        48_000,
        2,
        Limits {
            max_output_samples: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        batch.push(packet(&data)).unwrap_err().to_string(),
        "audio output exceeds sample budget"
    );
}

#[test]
fn noncanonical_initialization_and_invalid_output_budgets_are_refused() {
    let mut config = pcm();
    config.codec = "opus".into();
    config.packetization = "opus-packet-v1".into();
    assert!(Decoder::new(&config, 48_000, 2).is_err());
    for limits in [
        Limits {
            max_output_samples: 0,
            ..Default::default()
        },
        Limits {
            max_output_samples: usize::MAX,
            ..Default::default()
        },
        Limits {
            max_output_frames: 0,
            ..Default::default()
        },
        Limits {
            threads: 65,
            ..Default::default()
        },
    ] {
        assert!(Decoder::with_limits(&pcm(), 48_000, 2, limits).is_err());
    }
    for (rate, channels) in [(0, 2), (48_000, 0), (u32::MAX, 2), (48_000, u16::MAX)] {
        assert!(Decoder::new(&pcm(), rate, channels).is_err());
    }
}

#[test]
fn capture_resampling_rejects_small_buffers_without_buffering_input() {
    let mut capture = CaptureResampler::new(48_000).unwrap();
    let input = [0.25; 960];
    assert!(capture.convert(&input, &mut [0; 1]).is_err());
    let mut output = [0; 2048];
    let count = capture.convert(&input, &mut output).unwrap();
    assert_eq!(count, 960);
    assert!(output[..count].iter().all(|sample| *sample == 8192));
    assert!(CaptureResampler::new(0).is_err());
}

#[test]
fn nonfinite_pcm_cannot_reach_an_audio_device() {
    let mut config = pcm();
    config.codec = "pcm_f32le".into();
    let mut decoder = Decoder::new(&config, 48_000, 2).unwrap();
    let data = [f32::NAN.to_le_bytes(), 0_f32.to_le_bytes()].concat();
    assert!(decoder.push(packet(&data)).is_err());
    assert!(decoder.finish().is_err());
}
