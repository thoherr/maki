//! Audio import and `audio analyze`.

use crate::common::*;

/// Write a minimal valid PCM WAV file (44-byte RIFF header + silence).
/// lofty reads duration / sample rate / channels from it, so audio
/// metadata extraction can be tested without ffmpeg or fixture binaries.
fn create_test_wav(dir: &Path, name: &str, seconds: f64) -> PathBuf {
    let sample_rate: u32 = 8000;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let num_samples = (seconds * sample_rate as f64) as u32;
    let data_len = num_samples * (bits as u32 / 8) * channels as u32;
    let byte_rate = sample_rate * channels as u32 * bits as u32 / 8;
    let block_align = channels * bits / 8;

    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);

    create_test_file(dir, name, &bytes)
}

#[test]
fn import_audio_extracts_typed_metadata() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_wav(&root, "clip.wav", 3.0);

    maki()
        .current_dir(&root)
        .args(["import", root.join("clip.wav").to_str().unwrap()])
        .assert()
        .success();

    // Typed metadata flows to source_metadata and the duration filter
    let out = maki()
        .current_dir(&root)
        .args(["search", "type:audio duration:2-4", "--format", "{name}"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("clip"),
        "audio duration filter should match the imported clip"
    );

    // Sidecar carries the audio_* keys (source of truth)
    let show = maki()
        .current_dir(&root)
        .args(["search", "type:audio", "--format", "ids"])
        .output()
        .unwrap();
    let id = String::from_utf8_lossy(&show.stdout).trim().to_string();
    let details = maki()
        .current_dir(&root)
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&details.stdout).unwrap();
    let meta = &json["variants"][0]["source_metadata"];
    assert_eq!(meta["audio_duration"], "3.0");
    assert_eq!(meta["audio_sample_rate"], "8000");
    assert_eq!(meta["audio_channels"], "1");
}

#[cfg(unix)]
#[test]
fn audio_analyze_fills_key_and_bpm_via_stub_tools() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_wav(&root, "song.wav", 4.0);
    maki()
        .current_dir(&root)
        .args(["import", root.join("song.wav").to_str().unwrap()])
        .assert()
        .success();

    // Stub analyzers standing in for keyfinder-cli / beat_this
    let key_tool = root.join("stub-key.sh");
    std::fs::write(&key_tool, "#!/bin/sh\necho Em\n").unwrap();
    let bpm_tool = root.join("stub-bpm.sh");
    std::fs::write(
        &bpm_tool,
        "#!/bin/sh\nmkdir -p \"$3\"\nprintf '0.4\\n0.8\\n1.2\\n1.6\\n2.0\\n' > \"$3/song.beats\"\n",
    )
    .unwrap();
    for t in [&key_tool, &bpm_tool] {
        std::fs::set_permissions(t, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // `maki init` already writes the [audio] section with the default
    // tool names — point it at the stubs (appending a second [audio]
    // table would be a TOML parse error).
    let toml_path = root.join("maki.toml");
    let toml = std::fs::read_to_string(&toml_path)
        .unwrap()
        .replace(
            "key_command = \"keyfinder-cli\"",
            &format!("key_command = \"{}\"", key_tool.display()),
        )
        .replace(
            "bpm_command = \"beat_this\"",
            &format!("bpm_command = \"{}\"", bpm_tool.display()),
        );
    std::fs::write(&toml_path, toml).unwrap();

    maki()
        .current_dir(&root)
        .args(["audio", "analyze"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 analyzed"))
        .stdout(predicate::str::contains("1 keys set"))
        .stdout(predicate::str::contains("1 BPMs set"));

    // key: / bpm: filters hit the analyzed asset (0.4s beats = 150 BPM)
    maki()
        .current_dir(&root)
        .args(["search", "key:em", "--format", "{name}"])
        .assert()
        .success()
        .stdout(predicate::str::contains("song"));
    maki()
        .current_dir(&root)
        .args(["search", "bpm:140-160", "--format", "{name}"])
        .assert()
        .success()
        .stdout(predicate::str::contains("song"));

    // Second run skips (both fields present)
    maki()
        .current_dir(&root)
        .args(["audio", "analyze"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 skipped (already analyzed)"));
}

#[test]
fn audio_analyze_missing_tools_warns_and_exits_clean() {
    let dir = tempdir().unwrap();
    let root = init_catalog(dir.path());
    create_test_wav(&root, "clip.wav", 2.0);
    maki()
        .current_dir(&root)
        .args(["import", root.join("clip.wav").to_str().unwrap()])
        .assert()
        .success();

    let toml_path = root.join("maki.toml");
    let toml = std::fs::read_to_string(&toml_path)
        .unwrap()
        .replace("key_command = \"keyfinder-cli\"", "key_command = \"maki-nonexistent-key-tool\"")
        .replace("bpm_command = \"beat_this\"", "bpm_command = \"maki-nonexistent-bpm-tool\"");
    std::fs::write(&toml_path, toml).unwrap();

    maki()
        .current_dir(&root)
        .args(["audio", "analyze"])
        .assert()
        .success()
        .stderr(predicate::str::contains("key tool"))
        .stderr(predicate::str::contains("BPM tool"));
}
