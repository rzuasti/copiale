//! The core runs this binary's sandboxed video frame helper.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use copiale_core::video::FrameHelper;

fn helper() -> FrameHelper {
    FrameHelper::new(env!("CARGO_BIN_EXE_copiale"), ["--video-frame"])
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[tokio::test]
async fn videos_give_their_first_frame() {
    let png = helper()
        .first_frame(fixture("red-64x48.mp4"))
        .await
        .unwrap();

    let image = image::load_from_memory(&png).unwrap();
    assert_eq!((image.width(), image.height()), (64, 48));
}

#[tokio::test]
async fn anything_else_gives_none() {
    for input in [
        fixture("red-mpeg4.mp4"),
        b"not a video".to_vec(),
        Vec::new(),
    ] {
        assert!(helper().first_frame(input).await.is_err());
    }
}
