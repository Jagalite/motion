//! Authored descriptor/parser regressions. These are not runtime qualification
//! of FFmpeg, native catalog admission or the public route gate.
use super::*;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
};

fn manifest() -> String {
    "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"5\",BYTERANGE=\"20@0\"\n#EXTINF:4.000000,\n#EXT-X-BYTERANGE:40@20\n5\n#EXTINF:1.000000,\n#EXT-X-BYTERANGE:10@60\n5\n#EXT-X-ENDLIST\n".into()
}
fn outputs() -> (
    tempfile::TempDir,
    Arc<OutputFiles>,
    Arc<tokio::sync::Semaphore>,
) {
    let dir = tempfile::tempdir().unwrap();
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let root = dir.path().join("generation");
    std::fs::create_dir(&root).unwrap();
    let files = prepare(&root, capacity.clone().try_acquire_owned().unwrap()).unwrap();
    (dir, files, capacity)
}

#[test]
fn final_manifest_requires_vod_explicit_contiguous_bounded_ranges() {
    let valid = manifest();
    let parsed = parse_manifest(&valid).unwrap();
    assert_eq!(
        parsed.init,
        Range {
            offset: 0,
            length: 20
        }
    );
    assert_eq!(parsed.segments.len(), 2);
    assert!(parsed.ended);
    for invalid in [
        valid.replace("#EXT-X-PLAYLIST-TYPE:VOD\n", ""),
        valid.replace("#EXT-X-MEDIA-SEQUENCE:0", "#EXT-X-MEDIA-SEQUENCE:1"),
        valid.replace("40@20", "40@21"),
        valid.replace("40@20", "40"),
        valid.replace("40@20", "8388609@20"),
        valid.replace("20@0", "1048577@0"),
        valid.replace("20@0", "20@1"),
        valid.replace("4.000000", "NaN"),
        valid.replace("4.000000", "6.001"),
        valid.replace("\n5\n", "\n/private/replace.m4s\n"),
        valid.replace(
            "#EXT-X-ENDLIST\n",
            "#EXT-X-KEY:URI=\"outside\"\n#EXT-X-ENDLIST\n",
        ),
        format!("{valid}#EXTINF:1,\n"),
        format!("{valid}{}", "x".repeat(MAX_MANIFEST_BYTES as usize)),
    ] {
        assert!(parse_manifest(&invalid).is_none(), "{invalid}");
    }
    let mut excessive = valid[..valid.find("#EXTINF:").unwrap()].to_owned();
    for index in 0..17 {
        excessive.push_str(&format!(
            "#EXTINF:1.000000,\n#EXT-X-BYTERANGE:1@{}\n5\n",
            20 + index
        ));
    }
    excessive.push_str("#EXT-X-ENDLIST\n");
    assert!(parse_manifest(&excessive).is_none());
}

#[test]
fn anonymous_outputs_ignore_directory_replacement_and_detect_same_fd_mutation() {
    let (dir, files, capacity) = outputs();
    let root = dir.path().join("generation");
    // No media or manifest leaf exists to substitute, even before first pin.
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    files.media.write_all_at(b"owned-worker-bytes", 0).unwrap();
    std::fs::rename(&root, dir.path().join("old-generation")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("init.mp4"), b"replacement").unwrap();
    std::fs::write(root.join("seg0.m4s"), b"replacement").unwrap();
    std::fs::write(root.join("ffmpeg.m3u8"), manifest()).unwrap();
    let range = Range {
        offset: 0,
        length: 18,
    };
    let bytes = read_range(&files.media, &range, MAX_INIT_BYTES).unwrap_or_else(|error| {
        let (status, code, _) = error.parts();
        panic!("unexpected API error: {status} {code}")
    });
    assert_eq!(bytes, b"owned-worker-bytes");
    commit(&files, "init.mp4".into(), commitment(&range, &bytes)).unwrap_or_else(|error| {
        let (status, code, _) = error.parts();
        panic!("unexpected API error: {status} {code}")
    });
    assert_eq!(
        read_pinned(&files, "init.mp4").unwrap_or_else(|error| {
            let (status, code, _) = error.parts();
            panic!("unexpected API error: {status} {code}")
        }),
        bytes
    );
    assert!(capacity.clone().try_acquire_owned().is_err());
    files.media.write_all_at(b"changed", 0).unwrap();
    assert!(read_pinned(&files, "init.mp4").is_err());
    drop(files);
    assert!(capacity.try_acquire_owned().is_ok());
}

#[test]
fn anonymous_output_readers_use_positioned_io_not_shared_offsets() {
    let (_dir, files, _) = outputs();
    let mut writer = files.media.try_clone().unwrap();
    writer.write_all(b"0123456789").unwrap();
    writer.seek(SeekFrom::Start(8)).unwrap();
    let range = Range {
        offset: 2,
        length: 3,
    };
    assert_eq!(
        read_range(&files.media, &range, 3).unwrap_or_else(|error| {
            let (status, code, _) = error.parts();
            panic!("unexpected API error: {status} {code}")
        }),
        b"234"
    );
    assert_eq!(writer.stream_position().unwrap(), 8);
    let mut tail = [0; 2];
    writer.read_exact(&mut tail).unwrap();
    assert_eq!(&tail, b"89");
    assert!(
        read_range(
            &files.media,
            &Range {
                offset: u64::MAX,
                length: 1
            },
            8
        )
        .is_err()
    );
    assert!(
        read_range(
            &files.media,
            &Range {
                offset: 8,
                length: 3
            },
            8
        )
        .is_err()
    );
}

#[test]
fn private_fragment_bounds_include_the_shared_idr_parser_allocations() {
    fn boxed(kind: &[u8; 4], length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        bytes[..4].copy_from_slice(&(length as u32).to_be_bytes());
        bytes[4..8].copy_from_slice(kind);
        bytes
    }
    let good = [boxed(b"styp", 8), boxed(b"moof", 8), boxed(b"mdat", 8)].concat();
    assert!(bounded_fragment(&good));
    assert!(!bounded_fragment(
        &[boxed(b"moof", 256 * 1024 + 1), boxed(b"mdat", 8)].concat()
    ));
    assert!(!bounded_fragment(
        &[good.clone(), boxed(b"moof", 8)].concat()
    ));
    assert!(!bounded_fragment(
        &[good.clone(), boxed(b"free", 8).repeat(64)].concat()
    ));
    assert!(!bounded_fragment(&[0; 8]));
    assert!(!bounded_fragment(&good[..good.len() - 1]));
}

#[test]
fn native_arguments_have_no_output_path_or_temporary_playlist_rename() {
    let mut args = vec![
        "-i",
        "/dev/fd/3",
        "-c:v",
        "libx264",
        "-f",
        "hls",
        "/private/old/ffmpeg.m3u8",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    arguments(&mut args).unwrap();
    assert!(args.windows(2).any(|p| p == ["-hls_playlist_type", "vod"]));
    assert!(
        args.windows(2)
            .any(|p| p == ["-hls_flags", "single_file+independent_segments"])
    );
    assert_eq!(args.last().unwrap(), "/dev/fd/6");
    assert!(
        !args
            .iter()
            .any(|a| a.contains("/private") || a.contains("temp_file"))
    );
}

#[test]
fn actual_source_header_refuses_script_and_oversized_media_before_dispatch() {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"\0\0\0\x10ftypisom\0\0\0\0").unwrap();
    assert!(source_header(&file).is_ok());
    file.write_all_at(b"#EXTM3U\n", 0).unwrap();
    assert!(source_header(&file).is_err());
    file.write_all_at(b"\0\0\0\x10ftypisom\0\0\0\0", 0).unwrap();
    file.set_len(MAX_SOURCE_BYTES + 1).unwrap();
    assert!(source_header(&file).is_err());
}

#[tokio::test]
async fn owned_response_allocation_keeps_capacity_after_body_and_original_frame_drop() {
    use http_body_util::BodyExt;
    // This exercises the production byte-owner mechanism, not native admission.
    // The separate actual application fixture exercises the real native lease.
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let owner = Arc::new(());
    let weak = Arc::downgrade(&owner);
    let permit = capacity.clone().try_acquire_owned().unwrap();
    let mut body = owned_body(b"immutable-output".to_vec(), (owner, permit));
    let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let clone = data.clone();
    let slice = data.slice(2..8);
    assert!(body.frame().await.is_none());
    drop(body);
    drop(data);
    assert!(weak.upgrade().is_some());
    assert!(capacity.clone().try_acquire_owned().is_err());
    drop(clone);
    assert_eq!(&slice[..], b"mutabl");
    assert!(weak.upgrade().is_some());
    assert!(capacity.clone().try_acquire_owned().is_err());
    drop(slice);
    assert!(weak.upgrade().is_none());
    assert!(capacity.try_acquire_owned().is_ok());
}
