//! Thumbnails for image attachments, generated and decoded off the UI thread
//! and kept in a small LRU.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use fixtures::ImageAttachment;
use gpui_kit::{App, AppContext as _, Context, RenderImage, Task};
use image::{Frame, imageops::FilterType};

/// Longest thumbnail side, in physical pixels.
const THUMBNAIL_PX: u32 = 640;
/// Decoded thumbnails kept in memory.
const CAPACITY: usize = 64;

enum Slot {
    Loading(#[allow(dead_code)] Task<()>),
    Ready(Arc<RenderImage>),
}

#[derive(Default)]
pub struct ImageCache {
    slots: HashMap<u64, Slot>,
    /// Ready seeds, least recently used first.
    lru: VecDeque<u64>,
}

impl ImageCache {
    /// The thumbnail if decoded; otherwise starts decoding it.
    pub fn get(
        &mut self,
        attachment: ImageAttachment,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        match self.slots.get(&attachment.seed) {
            Some(Slot::Ready(image)) => {
                let image = image.clone();
                self.touch(attachment.seed);
                Some(image)
            }
            Some(Slot::Loading(_)) => None,
            None => {
                let task = cx.spawn(async move |this, cx| {
                    let image = cx
                        .background_spawn(async move { decode_thumbnail(attachment) })
                        .await;
                    this.update(cx, |cache, cx| cache.insert(attachment.seed, image, cx))
                        .ok();
                });
                self.slots.insert(attachment.seed, Slot::Loading(task));
                None
            }
        }
    }

    pub fn ready_count(&self) -> usize {
        self.lru.len()
    }

    fn insert(&mut self, seed: u64, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        self.slots.insert(seed, Slot::Ready(image));
        self.lru.push_back(seed);
        while self.lru.len() > CAPACITY {
            let evicted = self.lru.pop_front().expect("non-empty");
            if let Some(Slot::Ready(image)) = self.slots.remove(&evicted) {
                drop_from_gpu(image, cx);
            }
        }
        cx.notify();
    }

    fn touch(&mut self, seed: u64) {
        if let Some(pos) = self.lru.iter().position(|s| *s == seed) {
            self.lru.remove(pos);
            self.lru.push_back(seed);
        }
    }
}

fn drop_from_gpu(image: Arc<RenderImage>, cx: &mut App) {
    // Frees the sprite atlas entry in every window.
    cx.drop_image(image, None);
}

/// Generates the fixture PNG, decodes it, and downsamples to thumbnail size.
pub fn decode_thumbnail(attachment: ImageAttachment) -> Arc<RenderImage> {
    let png = attachment.encode_png();
    let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .expect("fixture PNGs are valid");
    let scale = THUMBNAIL_PX as f32 / attachment.width.max(attachment.height) as f32;
    let thumbnail = if scale < 1.0 {
        decoded.resize(
            (attachment.width as f32 * scale).round() as u32,
            (attachment.height as f32 * scale).round() as u32,
            FilterType::Triangle,
        )
    } else {
        decoded
    };
    let mut pixels = thumbnail.into_rgba8();
    // GPUI expects BGRA.
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new([Frame::new(pixels)]))
}
