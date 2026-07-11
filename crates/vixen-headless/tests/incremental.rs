use std::path::Path;
use std::process::Command;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

#[test]
fn incremental_cli_captures_distinct_before_and_after_frames() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/incremental.html");
    let url = url::Url::from_file_path(fixture).unwrap().to_string();
    let output_dir = tempfile::tempdir().unwrap();
    let requested = output_dir.path().join("capture.result.png");
    let frame_one = output_dir.path().join("capture.result-frame-1.png");
    let frame_two = output_dir.path().join("capture.result-frame-2.png");

    let result = Command::new(env!("CARGO_BIN_EXE_vixen-headless"))
        .args([
            "--url",
            &url,
            "--viewport",
            "160x120",
            "--screenshot",
        ])
        .arg(&requested)
        .args([
            "--eval",
            "const panel = document.querySelector('#panel'); panel.setAttribute('style', 'display:block;width:160px;height:120px;background-color:#1457d9;color:white'); panel.textContent = 'after'; 'mutated'",
            "--incremental",
        ])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"mutated\n");
    assert!(!requested.exists());
    assert!(frame_one.is_file());
    assert!(frame_two.is_file());

    let first_png = std::fs::read(frame_one).unwrap();
    let second_png = std::fs::read(frame_two).unwrap();
    assert_png_dimensions(&first_png, 160, 120);
    assert_png_dimensions(&second_png, 160, 120);
    assert_ne!(first_png, second_png);
}

fn assert_png_dimensions(png: &[u8], width: u32, height: u32) {
    assert!(png.starts_with(PNG_SIGNATURE));
    assert!(png.len() >= 24);
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), width);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), height);
}
