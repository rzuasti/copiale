//! Thumbnails for image attachments, decoded off the UI thread and kept in a
//! small LRU.

use std::collections::{HashMap, VecDeque};

use fixtures::ImageAttachment;
use iced::widget::image::Handle;
use image::imageops::FilterType;

/// Longest thumbnail side, in physical pixels.
const THUMBNAIL_PX: u32 = 640;
/// Decoded thumbnails kept in memory.
const CAPACITY: usize = 64;

enum Slot {
    Loading,
    Ready(Handle),
}

#[derive(Default)]
pub struct ImageCache {
    slots: HashMap<u64, Slot>,
    /// Ready seeds, least recently used first.
    lru: VecDeque<u64>,
}

impl ImageCache {
    pub fn get(&self, seed: u64) -> Option<&Handle> {
        match self.slots.get(&seed) {
            Some(Slot::Ready(handle)) => Some(handle),
            _ => None,
        }
    }

    /// Marks `attachment` as loading; `false` if already loading or ready.
    pub fn request(&mut self, attachment: ImageAttachment) -> bool {
        if let Some(Slot::Ready(_)) = self.slots.get(&attachment.seed) {
            self.touch(attachment.seed);
            return false;
        }
        if self.slots.contains_key(&attachment.seed) {
            return false;
        }
        self.slots.insert(attachment.seed, Slot::Loading);
        true
    }

    pub fn insert(&mut self, seed: u64, handle: Handle) {
        self.slots.insert(seed, Slot::Ready(handle));
        self.lru.push_back(seed);
        while self.lru.len() > CAPACITY {
            // iced frees the GPU copy once a handle is no longer drawn.
            let evicted = self.lru.pop_front().expect("non-empty");
            self.slots.remove(&evicted);
        }
    }

    pub fn ready_count(&self) -> usize {
        self.lru.len()
    }

    fn touch(&mut self, seed: u64) {
        if let Some(pos) = self.lru.iter().position(|s| *s == seed) {
            self.lru.remove(pos);
            self.lru.push_back(seed);
        }
    }
}

/// Generates the fixture PNG, decodes it, and downsamples to thumbnail size.
pub fn decode_thumbnail(attachment: ImageAttachment) -> Handle {
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
    let pixels = thumbnail.into_rgba8();
    Handle::from_rgba(pixels.width(), pixels.height(), pixels.into_raw())
}
