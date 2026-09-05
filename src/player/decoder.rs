//! Owns decoder preference, malformed H.264 fallback, and missing-decoder diagnostics.

use std::sync::{Mutex, MutexGuard};

use gstreamer as gst;
use gstreamer::prelude::*;

/// Hardware decoders proven on the target machine. GStreamer's libav
/// decoders rank at `Primary`, while VA-API and NVDEC can rank at
/// `Primary + 1` and these QSV factories normally rank lower. Prefer QSV
/// for streams its caps accept and leave other installed decoders as the
/// automatic fallback for streams the iGPU cannot decode (FR-10.1).
const INTEL_VIDEO_DECODERS: &[(&str, &str)] = &[
    ("qsvh264dec", "avdec_h264"),
    ("qsvh265dec", "avdec_h265"),
    ("qsvvp9dec", "avdec_vp9"),
    ("qsvjpegdec", "avdec_mjpeg"),
];
/// True when the coded frame exceeds the size limit the stream itself
/// declares. Unknown levels stay on the normal hardware
/// path; this guard only acts on a demonstrable metadata contradiction.
pub(super) fn h264_exceeds_declared_level(caps: &gst::CapsRef) -> bool {
    let Some(structure) = caps.iter().find(|s| s.name() == "video/x-h264") else {
        return false;
    };
    let Ok(level) = structure.get::<String>("level") else {
        return false;
    };
    let Some(max_frame_mbs) = h264_level_max_frame_mbs(&level) else {
        return false;
    };
    let (Ok(width), Ok(height)) = (
        structure.get::<i32>("width"),
        structure.get::<i32>("height"),
    ) else {
        return false;
    };
    if width <= 0 || height <= 0 {
        return false;
    }

    let (Ok(width), Ok(height)) = (u64::try_from(width), u64::try_from(height)) else {
        return false;
    };
    let frame_mbs = width.div_ceil(16) * height.div_ceil(16);
    frame_mbs > max_frame_mbs
}

/// Compact encoded-stream facts for diagnostics. Raw caps dumps are noisy and
/// unstable across GStreamer versions; these fields explain the decoder choice
/// and the common hardware-limit failures without logging from the render path.
pub(super) fn video_stream_summary(caps: &gst::CapsRef) -> Option<String> {
    let structure = caps.structure(0)?;
    let mut summary = structure.name().to_string();

    for field in ["profile", "level"] {
        if let Ok(value) = structure.get::<String>(field) {
            summary.push_str(&format!(" {field}={value}"));
        }
    }
    if let (Ok(width), Ok(height)) = (
        structure.get::<i32>("width"),
        structure.get::<i32>("height"),
    ) {
        summary.push_str(&format!(" {width}x{height}"));
    }
    if let Ok(rate) = structure.get::<gst::Fraction>("framerate") {
        summary.push_str(&format!(" {}/{} fps", rate.numer(), rate.denom()));
    }

    Some(summary)
}

/// H.264 Annex A `MaxFS` in macroblocks.
fn h264_level_max_frame_mbs(level: &str) -> Option<u64> {
    Some(match level {
        "1" | "1b" => 99,
        "1.1" | "1.2" | "1.3" | "2" => 396,
        "2.1" => 792,
        "2.2" | "3" => 1_620,
        "3.1" => 3_600,
        "3.2" => 5_120,
        "4" | "4.1" => 8_192,
        "4.2" => 8_704,
        "5" => 22_080,
        "5.1" | "5.2" => 36_864,
        "6" | "6.1" | "6.2" => 139_264,
        _ => return None,
    })
}

/// The stream collection is posted synchronously before `decodebin3`
/// chooses a decoder. Temporarily lowering QSV's rank here ensures its
/// cached candidate list is built without QSV for only this malformed
/// stream. Restore the process-wide rank as soon as libav is constructed.
#[derive(Default)]
pub(super) struct DecoderFallback {
    disabled_rank: Option<gst::Rank>,
}

impl DecoderFallback {
    pub(super) fn bypass_qsv_h264(&mut self, caps: &gst::CapsRef) -> Option<&'static str> {
        if self.disabled_rank.is_some() || !h264_exceeds_declared_level(caps) {
            return None;
        }
        let fallback = gst::ElementFactory::find("avdec_h264")?;
        if fallback.rank() == gst::Rank::NONE {
            return None;
        }
        let factory = gst::ElementFactory::find("qsvh264dec")?;
        let rank = factory.rank();
        if rank == gst::Rank::NONE {
            return None;
        }
        factory.set_rank(gst::Rank::NONE);
        self.disabled_rank = Some(rank);
        Some("avdec_h264")
    }

    pub(super) fn restore(&mut self) {
        let Some(rank) = self.disabled_rank.take() else {
            return;
        };
        if let Some(factory) = gst::ElementFactory::find("qsvh264dec") {
            factory.set_rank(rank);
        }
    }
}

pub(super) fn lock_decoder_fallback(
    state: &Mutex<DecoderFallback>,
) -> MutexGuard<'_, DecoderFallback> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Give the verified Intel decoder priority over standard `Primary + 1`
/// hardware and `Primary` software decoders. `None` means preserve an
/// explicit disable (`Rank::None`) or a choice already ranked above ours.
pub(super) fn preferred_hardware_rank(current: gst::Rank) -> Option<gst::Rank> {
    let preferred = gst::Rank::PRIMARY + 2;
    (current != gst::Rank::NONE && current < preferred).then_some(preferred)
}

/// Change only this process's registry. Missing factories are expected on
/// other Intel generations, and an explicitly disabled factory stays off.
/// This remains after lazy `gst::init` so image-only startup does not load
/// GStreamer (NFR-1.1).
pub(super) fn prefer_intel_video_decoders() {
    for (name, _) in INTEL_VIDEO_DECODERS {
        let Some(factory) = gst::ElementFactory::find(name) else {
            continue;
        };
        let current = factory.rank();
        let Some(preferred) = preferred_hardware_rank(current) else {
            continue;
        };
        factory.set_rank(preferred);
        crate::applog!("player: prefer {name} ({current} -> {preferred})");
    }
}

/// GStreamer can keep playing the audio half of a file when its video
/// decoder is missing. That is a successful pipeline state rather than
/// an `Error` message, so recognise the accompanying `missing-plugin`
/// element message and surface the broken video playback (FR-10.6).
/// Missing audio and subtitle decoders do not make the picture unusable.
pub(super) fn missing_video_decoder(structure: &gst::StructureRef) -> Option<String> {
    missing_decoder_matching(structure, |media_type| media_type.starts_with("video/"))
}

pub(super) fn missing_subtitle_decoder(structure: &gst::StructureRef) -> Option<String> {
    missing_decoder_matching(structure, |media_type| {
        media_type.starts_with("text/")
            || media_type.starts_with("subpicture/")
            || media_type.starts_with("closedcaption/")
            || media_type.starts_with("application/x-subtitle")
    })
}

fn missing_decoder_matching(
    structure: &gst::StructureRef,
    matches_media_type: impl Fn(&str) -> bool,
) -> Option<String> {
    if structure.name() != "missing-plugin"
        || structure.get::<String>("type").ok().as_deref() != Some("decoder")
    {
        return None;
    }

    let caps = structure.get::<gst::Caps>("detail").ok()?;
    if !caps
        .iter()
        .any(|candidate| matches_media_type(candidate.name().as_str()))
    {
        return None;
    }

    Some(
        structure
            .get::<String>("name")
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| caps.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use gstreamer::prelude::PluginFeatureExtManual;

    use super::*;

    #[test]
    fn hardware_preference_preserves_explicit_rank_choices() {
        assert_eq!(
            preferred_hardware_rank(gst::Rank::MARGINAL),
            Some(gst::Rank::PRIMARY + 2)
        );
        assert_eq!(
            preferred_hardware_rank(gst::Rank::PRIMARY + 1),
            Some(gst::Rank::PRIMARY + 2)
        );
        assert_eq!(preferred_hardware_rank(gst::Rank::NONE), None);
        assert_eq!(preferred_hardware_rank(gst::Rank::PRIMARY + 3), None);
    }

    #[test]
    fn installed_intel_video_decoders_outrank_standard_hardware() {
        gst::init().unwrap();
        prefer_intel_video_decoders();

        for (name, _) in INTEL_VIDEO_DECODERS {
            let Some(factory) = gst::ElementFactory::find(name) else {
                continue;
            };
            if factory.rank() != gst::Rank::NONE {
                assert!(factory.rank() > gst::Rank::PRIMARY + 1, "{name}");
            }
        }
    }

    #[test]
    fn invalid_h264_level_temporarily_disables_qsv_h264() {
        gst::init().unwrap();
        prefer_intel_video_decoders();
        let Some(factory) = gst::ElementFactory::find("qsvh264dec") else {
            return;
        };
        let Some(fallback) = gst::ElementFactory::find("avdec_h264") else {
            return;
        };
        if fallback.rank() == gst::Rank::NONE {
            return;
        }
        let caps = gst::Caps::builder("video/x-h264")
            .field("level", "4")
            .field("width", 4_382i32)
            .field("height", 3_500i32)
            .field("framerate", gst::Fraction::new(30, 1))
            .build();
        let original = factory.rank();
        let mut fallback = DecoderFallback::default();

        assert!(h264_exceeds_declared_level(&caps));
        assert_eq!(fallback.bypass_qsv_h264(&caps), Some("avdec_h264"));
        assert_eq!(factory.rank(), gst::Rank::NONE);
        assert_eq!(fallback.bypass_qsv_h264(&caps), None);
        fallback.restore();
        assert_eq!(factory.rank(), original);
    }

    #[test]
    fn frame_within_h264_level_keeps_hardware_at_high_framerates() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-h264")
            .field("level", "4")
            .field("width", 1_120i32)
            .field("height", 1_632i32)
            .field("framerate", gst::Fraction::new(60, 1))
            .build();

        assert!(!h264_exceeds_declared_level(&caps));
    }

    #[test]
    fn video_diagnostics_include_decoder_relevant_stream_facts() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-h264")
            .field("profile", "high")
            .field("level", "4.1")
            .field("width", 1_920i32)
            .field("height", 1_080i32)
            .field("framerate", gst::Fraction::new(30, 1))
            .build();

        assert_eq!(
            video_stream_summary(&caps),
            Some("video/x-h264 profile=high level=4.1 1920x1080 30/1 fps".to_string())
        );
        assert_eq!(
            video_stream_summary(&gst::Caps::builder("video/x-vp9").build()),
            Some("video/x-vp9".to_string())
        );
    }

    #[test]
    fn a_missing_video_decoder_is_not_mistaken_for_successful_playback() {
        gst::init().unwrap();
        let message = gst::Structure::builder("missing-plugin")
            .field("type", "decoder")
            .field("detail", gst::Caps::builder("video/x-h264").build())
            .field("name", "H.264 High 10 decoder")
            .build();

        assert_eq!(
            missing_video_decoder(&message),
            Some("H.264 High 10 decoder".to_owned())
        );
    }

    #[test]
    fn missing_non_video_decoders_do_not_hide_a_playable_picture() {
        gst::init().unwrap();
        for media_type in ["audio/mpeg", "text/x-raw"] {
            let message = gst::Structure::builder("missing-plugin")
                .field("type", "decoder")
                .field("detail", gst::Caps::builder(media_type).build())
                .field("name", "optional stream decoder")
                .build();

            assert_eq!(missing_video_decoder(&message), None);
        }
    }

    #[test]
    fn missing_text_decoder_is_reported_as_a_subtitle_failure() {
        gst::init().unwrap();
        let message = gst::Structure::builder("missing-plugin")
            .field("type", "decoder")
            .field("detail", gst::Caps::builder("text/x-raw").build())
            .field("name", "subtitle decoder")
            .build();

        assert_eq!(
            missing_subtitle_decoder(&message),
            Some("subtitle decoder".to_string())
        );
        assert_eq!(missing_video_decoder(&message), None);
    }
}
