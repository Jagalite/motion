//! Minimal fragmented-MP4 inspection of live output: whether a segment's first
//! video sample is an H.264 IDR access unit. Anything not understood is `None`,
//! which callers treat as unproven.
use std::io::{Read, Seek, SeekFrom};

/// Largest `moof` read from a segment.
const READ_LIMIT: u64 = 4 * 1024 * 1024;

/// The video track of an init segment: its id and the AVCC NAL length size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTrack {
    pub track_id: u32,
    pub length_size: usize,
}

/// Iterate the boxes in `data`: (type, payload).
fn boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let kind: [u8; 4] = data[4..8].try_into().ok()?;
        let (header, size) = match u32::from_be_bytes(data[..4].try_into().ok()?) {
            0 => (8, data.len()),
            1 => (
                16,
                usize::try_from(u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)).ok()?,
            ),
            size => (8, size as usize),
        };
        if size < header || size > data.len() {
            data = &[];
            return None;
        }
        let payload = &data[header..size];
        data = &data[size..];
        Some((kind, payload))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, p)| p)
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// The single H.264 video track described by an init segment.
pub fn video_track(init: &[u8]) -> Option<VideoTrack> {
    let moov = boxes(init).find(|(k, _)| k == b"moov")?.1;
    let mut found = None;
    for (_, trak) in boxes(moov).filter(|(k, _)| k == b"trak") {
        let mdia = child(trak, b"mdia")?;
        if child(mdia, b"hdlr")?.get(8..12)? != b"vide" {
            continue;
        }
        let tkhd = child(trak, b"tkhd")?;
        let track_id = match tkhd.first()? {
            0 => u32_at(tkhd, 12)?,
            1 => u32_at(tkhd, 20)?,
            _ => return None,
        };
        let stsd = child(child(child(mdia, b"minf")?, b"stbl")?, b"stsd")?;
        let mut entries = boxes(stsd.get(8..)?);
        let (kind, entry) = entries.next()?;
        if !matches!(&kind, b"avc1" | b"avc3") || entries.next().is_some() {
            return None;
        }
        // Visual sample entry fields precede its child boxes.
        let avcc = child(entry.get(78..)?, b"avcC")?;
        let length_size = usize::from(avcc.get(4)? & 3) + 1;
        if length_size == 3 || found.is_some() {
            return None;
        }
        found = Some(VideoTrack {
            track_id,
            length_size,
        });
    }
    found
}

/// Offset (from the start of the segment) and size of the first sample of
/// `track_id` in a `moof` that starts at `moof_start`. Only explicit data
/// addressing (a base data offset, or the moof as base) is accepted.
fn first_sample(moof: &[u8], moof_start: u64, track_id: u32) -> Option<Option<(u64, u64)>> {
    for (_, traf) in boxes(moof).filter(|(k, _)| k == b"traf") {
        let tfhd = child(traf, b"tfhd")?;
        if u32_at(tfhd, 4)? != track_id {
            continue;
        }
        let flags = u32_at(tfhd, 0)? & 0x00ff_ffff;
        let mut at = 8;
        let base = if flags & 0x1 != 0 {
            at += 8;
            u64_at(tfhd, 8)?
        } else if flags & 0x2_0000 != 0 {
            moof_start
        } else {
            // Implicit addressing continues from the previous fragment's data.
            return None;
        };
        if flags & 0x2 != 0 {
            at += 4;
        }
        if flags & 0x8 != 0 {
            at += 4;
        }
        let default_size = (flags & 0x10 != 0).then(|| u32_at(tfhd, at)).flatten();
        let trun = child(traf, b"trun")?;
        let flags = u32_at(trun, 0)? & 0x00ff_ffff;
        if u32_at(trun, 4)? == 0 {
            return None;
        }
        let mut at = 8;
        let offset = if flags & 0x1 != 0 {
            at += 4;
            i64::from(u32_at(trun, 8)? as i32)
        } else {
            0
        };
        if flags & 0x4 != 0 {
            at += 4;
        }
        if flags & 0x100 != 0 {
            at += 4;
        }
        let size = if flags & 0x200 != 0 {
            u32_at(trun, at)?
        } else {
            default_size?
        };
        let start = u64::try_from(i64::try_from(base).ok()?.checked_add(offset)?).ok()?;
        return Some(Some((start, u64::from(size))));
    }
    Some(None)
}

/// Whether the first video sample of `segment` is an IDR access unit: its first
/// slice NAL unit is type 5. `None` when the segment cannot be read as expected.
pub fn starts_with_idr(track: VideoTrack, segment: &mut (impl Read + Seek)) -> Option<bool> {
    let end = segment.seek(SeekFrom::End(0)).ok()?;
    let mut position = 0u64;
    let mut sample = None;
    let mut media = Vec::new();
    // Top-level boxes: the first video sample's range, and every mdat payload.
    while position < end {
        if position + 8 > end {
            return None;
        }
        segment.seek(SeekFrom::Start(position)).ok()?;
        let mut header = [0u8; 16];
        segment.read_exact(&mut header[..8]).ok()?;
        let (header_len, size) = match u32_at(&header, 0)? {
            1 => {
                segment.read_exact(&mut header[8..]).ok()?;
                (16, u64_at(&header, 8)?)
            }
            0 => (8, end - position),
            size => (8, u64::from(size)),
        };
        if size < header_len || position.checked_add(size)? > end {
            return None;
        }
        match &header[4..8] {
            b"moof" if sample.is_none() => {
                if size > READ_LIMIT {
                    return None;
                }
                let mut moof = vec![0u8; usize::try_from(size - header_len).ok()?];
                segment.read_exact(&mut moof).ok()?;
                sample = first_sample(&moof, position, track.track_id)?;
            }
            b"mdat" => media.push((position + header_len, position + size)),
            _ => {}
        }
        position += size;
    }
    let (start, size) = sample?;
    let finish = start.checked_add(size)?;
    // The sample must be media data, not metadata or padding.
    if size == 0
        || !media
            .iter()
            .any(|&(from, to)| from <= start && finish <= to)
    {
        return None;
    }
    // Walk every NAL unit of the sample: the framing must account for it exactly.
    segment.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, segment);
    let mut prefix = [0u8; 5];
    let prefix = &mut prefix[..=track.length_size];
    let mut at = start;
    let mut first_slice = None;
    while at < finish {
        reader.read_exact(prefix).ok()?;
        let length = prefix[..track.length_size]
            .iter()
            .fold(0u64, |n, b| (n << 8) | u64::from(*b));
        let header = prefix[track.length_size];
        let nal_type = header & 0x1f;
        // Non-empty, within the sample, forbidden bit clear; an IDR is a reference.
        at = at
            .checked_add(track.length_size as u64)?
            .checked_add(length)?;
        if length == 0 || at > finish || header & 0x80 != 0 || (nal_type == 5 && header & 0x60 == 0)
        {
            return None;
        }
        if first_slice.is_none() && (1..=5).contains(&nal_type) {
            first_slice = Some(nal_type == 5);
        }
        reader.seek_relative(i64::try_from(length - 1).ok()?).ok()?;
    }
    first_slice
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn trak(track_id: u32, handler: &[u8; 4], entry: &[u8; 4], length_size: u8) -> Vec<u8> {
        let mut tkhd = vec![0u8; 12];
        tkhd.extend(track_id.to_be_bytes());
        tkhd.extend([0u8; 64]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend([0u8; 12]);
        let avcc = mp4_box(b"avcC", &[1, 100, 0, 40, 0xfc | (length_size - 1)]);
        let mut sample_entry = vec![0u8; 78];
        sample_entry.extend(avcc);
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(mp4_box(entry, &sample_entry));
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let minf = mp4_box(b"minf", &stbl);
        let mut mdia = mp4_box(b"hdlr", &hdlr);
        mdia.extend(minf);
        let mut trak = mp4_box(b"tkhd", &tkhd);
        trak.extend(mp4_box(b"mdia", &mdia));
        mp4_box(b"trak", &trak)
    }

    fn init(traks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = mp4_box(b"ftyp", b"iso5\0\0\0\0");
        out.extend(mp4_box(b"moov", &traks.concat()));
        out
    }

    /// A segment with an audio traf (track 2) before the video traf (track 1);
    /// `nals` is the first video sample's NAL types, each with a 3-byte body.
    fn segment(nals: &[u8]) -> Vec<u8> {
        let mut video_sample = Vec::new();
        for nal in nals {
            video_sample.extend(4u32.to_be_bytes());
            video_sample.extend([*nal, 0xaa, 0xbb, 0xcc]);
        }
        let audio_sample = [0x21u8; 10];
        let traf = |track: u32, size: usize, offset: u32| {
            let mut tfhd = 0x0002_0000u32.to_be_bytes().to_vec();
            tfhd.extend(track.to_be_bytes());
            let mut trun = 0x0000_0201u32.to_be_bytes().to_vec();
            trun.extend(1u32.to_be_bytes());
            trun.extend(offset.to_be_bytes());
            trun.extend((size as u32).to_be_bytes());
            let mut traf = mp4_box(b"tfhd", &tfhd);
            traf.extend(mp4_box(b"trun", &trun));
            mp4_box(b"traf", &traf)
        };
        // Both trafs have the same length; offsets are from the moof start.
        let traf_len = traf(0, 0, 0).len();
        let moof_len = 8 + 2 * traf_len;
        let audio_at = moof_len + 8;
        let video_at = audio_at + audio_sample.len();
        let mut moof = traf(2, audio_sample.len(), audio_at as u32);
        moof.extend(traf(1, video_sample.len(), video_at as u32));
        // Offsets are relative to the moof, which follows styp.
        let mut out = mp4_box(b"styp", b"msdh\0\0\0\0");
        out.extend(mp4_box(b"moof", &moof));
        out.extend(mp4_box(
            b"mdat",
            &[&audio_sample[..], &video_sample[..]].concat(),
        ));
        out
    }

    fn check(nals: &[u8]) -> Option<bool> {
        let track = video_track(&init(&[
            trak(2, b"soun", b"mp4a", 4),
            trak(1, b"vide", b"avc1", 4),
        ]))?;
        starts_with_idr(track, &mut std::io::Cursor::new(segment(nals)))
    }

    #[test]
    fn finds_the_video_track_and_length_size() {
        let init = init(&[trak(2, b"soun", b"mp4a", 4), trak(7, b"vide", b"avc3", 2)]);
        assert_eq!(
            video_track(&init),
            Some(VideoTrack {
                track_id: 7,
                length_size: 2
            })
        );
        assert_eq!(video_track(&init_with_hevc()), None);
    }

    fn init_with_hevc() -> Vec<u8> {
        init(&[trak(1, b"vide", b"hvc1", 4)])
    }

    #[test]
    fn units_after_the_first_slice_are_validated() {
        let track = VideoTrack {
            track_id: 1,
            length_size: 4,
        };
        // IDR slice, then a second slice whose length overruns the sample.
        let mut bytes = segment(&[0x65, 0x65]);
        let second = bytes.len() - 8;
        assert_eq!(
            starts_with_idr(track, &mut std::io::Cursor::new(bytes.clone())),
            Some(true)
        );
        bytes[second..second + 4].copy_from_slice(&9u32.to_be_bytes());
        assert_eq!(
            starts_with_idr(track, &mut std::io::Cursor::new(bytes)),
            None
        );
    }

    #[test]
    fn idr_after_parameter_sets_is_independent() {
        // AUD, SPS, PPS, SEI, IDR slice.
        assert_eq!(check(&[9, 7, 8, 6, 0x65]), Some(true));
    }

    #[test]
    fn non_idr_keyframe_is_not_independent() {
        // A recovery-point SEI before a non-IDR I slice: an open-GOP keyframe.
        assert_eq!(check(&[9, 6, 0x41]), Some(false));
    }

    #[test]
    fn malformed_nal_framing_is_unproven() {
        let track = VideoTrack {
            track_id: 1,
            length_size: 4,
        };
        let run = |bytes: Vec<u8>| starts_with_idr(track, &mut std::io::Cursor::new(bytes));
        let base = segment(&[0x65]);
        // The video sample is the last 8 bytes: a 4-byte length, then the NAL.
        let length_at = base.len() - 8;
        for length in [0u32, 5, u32::MAX] {
            let mut bytes = base.clone();
            bytes[length_at..length_at + 4].copy_from_slice(&length.to_be_bytes());
            assert_eq!(run(bytes), None, "length {length}");
        }
        // Forbidden-zero bit set, or no reference bits, on an IDR header.
        for header in [0xe5, 0x05] {
            let mut bytes = base.clone();
            bytes[length_at + 4] = header;
            assert_eq!(run(bytes), None, "header {header:#x}");
        }
        // The sample range is not inside an mdat.
        let mut bytes = base.clone();
        let mdat = bytes.windows(4).rposition(|w| w == b"mdat").unwrap();
        bytes[mdat..mdat + 4].copy_from_slice(b"free");
        assert_eq!(run(bytes), None);
        // Implicit data addressing (neither base flag) is refused.
        let mut bytes = base;
        let tfhd = bytes.windows(4).rposition(|w| w == b"tfhd").unwrap();
        bytes[tfhd + 4..tfhd + 8].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(run(bytes), None);
    }

    #[test]
    fn samples_without_slices_or_broken_offsets_are_unproven() {
        assert_eq!(check(&[9, 6]), None);
        let track = VideoTrack {
            track_id: 1,
            length_size: 4,
        };
        let mut truncated = segment(&[0x65]);
        truncated.truncate(truncated.len() - 3);
        assert_eq!(
            starts_with_idr(track, &mut std::io::Cursor::new(truncated)),
            None
        );
        // A track the segment does not carry.
        let other = VideoTrack {
            track_id: 9,
            length_size: 4,
        };
        assert_eq!(
            starts_with_idr(other, &mut std::io::Cursor::new(segment(&[0x65]))),
            None
        );
    }
}
