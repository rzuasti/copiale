//! The app runs this binary's sandboxed video helper.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::time::{Duration, Instant};

use copiale_core::video::{Frame, Player, VideoHelper};

fn helper() -> VideoHelper {
    VideoHelper::new(env!("CARGO_BIN_EXE_copiale"))
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

/// The next frame `player` shows, waiting up to a few seconds.
fn next_frame(player: &mut Player) -> Option<Frame> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(frame) = player.frame(Instant::now()) {
            return Some(frame);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// BGRA in the frame's middle.
fn middle(frame: &Frame) -> [u8; 4] {
    let at = frame.pixels.len() / 2 / 4 * 4;
    frame.pixels[at..at + 4].try_into().unwrap()
}

#[test]
fn videos_play_and_seek() {
    let mut player = Player::start(&helper(), fixture("red-blue-aac.mp4"), false).unwrap();

    let first = next_frame(&mut player).unwrap();
    assert_eq!((first.width, first.height), (64, 48));
    let [b, _, r, _] = middle(&first);
    assert!(r > 200 && b < 60);
    let status = player.status(Instant::now());
    assert!(status.playing && !status.failed);
    assert!(
        status
            .duration
            .is_some_and(|d| d > Duration::from_millis(1900))
    );

    player.toggle(Instant::now());
    player.seek(Duration::from_millis(1500));
    let [b, _, r, _] = middle(&next_frame(&mut player).unwrap());
    assert!(b > 200 && r < 60);
    assert!(!player.status(Instant::now()).playing);

    // Plays to the end.
    player.toggle(Instant::now());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !player.status(Instant::now()).ended && Instant::now() < deadline {
        player.frame(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = player.status(Instant::now());
    assert!(status.ended && !status.playing, "{status:?}");
}

#[test]
fn what_it_cant_play_fails() {
    let mut player = Player::start(&helper(), fixture("red-mpeg4.mp4"), false).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !player.status(Instant::now()).failed && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(player.status(Instant::now()).failed);
    assert!(player.frame(Instant::now()).is_none());
}
