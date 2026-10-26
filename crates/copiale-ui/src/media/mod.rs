//! Downloaded images and blurred previews, decoded for GPUI and kept a
//! while.

mod blurhash;
mod decode;

use std::collections::HashMap;
use std::sync::Arc;

use copiale_core::AttachmentId;
use copiale_core::video::VideoHelper;
use gpui_kit::{Global, RenderImage};
use image::{Frame, RgbaImage};

pub use decode::{Decoded, decode};

/// Pictures kept decoded; the least recently drawn go first.
const MAX_PICTURES: usize = 48;
/// Blurred previews kept.
const MAX_PREVIEWS: usize = 256;
/// A blurred preview's side, in pixels; GPUI scales it.
const PREVIEW_SIDE: u32 = 32;

/// Plays videos in the viewer; unset where the helper can't run.
#[derive(Clone)]
pub struct VideoPlayback {
    pub helper: VideoHelper,
    /// Plays their sound too.
    pub sound: bool,
}

impl Global for VideoPlayback {}

/// What a picture is decoded for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Use {
    /// The log's box.
    Thumbnail,
    /// The viewer.
    Full,
}

impl Use {
    /// The square, in pixels, it's scaled down to fit: room for HiDPI.
    pub fn fit(self) -> u32 {
        match self {
            Self::Thumbnail => 720,
            Self::Full => 3072,
        }
    }
}

/// A decoded picture and its size in pixels.
#[derive(Clone)]
pub struct Picture {
    pub image: Arc<RenderImage>,
    pub width: u32,
    pub height: u32,
}

impl Picture {
    pub fn new(decoded: Decoded) -> Self {
        Self {
            image: Arc::new(RenderImage::new(decoded.frames)),
            width: decoded.width,
            height: decoded.height,
        }
    }
}

#[derive(Clone)]
pub enum Slot {
    /// Asked of the core, or decoding.
    Loading,
    Ready(Picture),
    /// Not an image this draws, or not here after all.
    Failed,
}

/// Pictures by attachment and use, and previews by blurhash.
#[derive(Default)]
pub struct Pictures {
    slots: HashMap<(AttachmentId, Use), (Slot, u64)>,
    previews: HashMap<String, (Option<Arc<RenderImage>>, u64)>,
    /// Previews no longer kept, for GPUI to drop.
    dropped: Vec<Arc<RenderImage>>,
    /// Counts lookups, to find the least recently used.
    clock: u64,
}

impl Pictures {
    /// The picture for `id` and `use_`, if asked for.
    pub fn get(&mut self, id: AttachmentId, use_: Use) -> Option<Slot> {
        self.clock += 1;
        let (slot, used) = self.slots.get_mut(&(id, use_))?;
        *used = self.clock;
        Some(slot.clone())
    }

    /// Notes the picture for `id` and `use_` is loading; `false` if it
    /// already is, or was loaded.
    pub fn want(&mut self, id: AttachmentId, use_: Use) -> bool {
        if self.slots.contains_key(&(id, use_)) {
            return false;
        }
        self.clock += 1;
        self.slots.insert((id, use_), (Slot::Loading, self.clock));
        true
    }

    /// What `id` is loading for.
    pub fn loading(&self, id: AttachmentId) -> Vec<Use> {
        [Use::Thumbnail, Use::Full]
            .into_iter()
            .filter(|use_| matches!(self.slots.get(&(id, *use_)), Some((Slot::Loading, _))))
            .collect()
    }

    /// Stores what loading gave, if it's still wanted; returns pictures no
    /// longer kept, for GPUI to drop.
    pub fn finish(&mut self, id: AttachmentId, use_: Use, slot: Slot) -> Vec<Arc<RenderImage>> {
        let Some((kept, _)) = self.slots.get_mut(&(id, use_)) else {
            return picture(slot).into_iter().collect();
        };
        *kept = slot;
        self.evict()
    }

    /// Drops the viewer's pictures but `keep`; returns them for GPUI.
    pub fn forget_full(&mut self, keep: Option<AttachmentId>) -> Vec<Arc<RenderImage>> {
        let gone: Vec<_> = self
            .slots
            .keys()
            .filter(|(id, use_)| *use_ == Use::Full && Some(*id) != keep)
            .copied()
            .collect();
        gone.into_iter()
            .filter_map(|key| self.slots.remove(&key))
            .filter_map(|(slot, _)| picture(slot))
            .collect()
    }

    /// `hash` drawn small, or `None` if it isn't a blurhash.
    pub fn preview(&mut self, hash: &str) -> Option<Arc<RenderImage>> {
        self.clock += 1;
        if let Some((image, used)) = self.previews.get_mut(hash) {
            *used = self.clock;
            return image.clone();
        }
        if self.previews.len() >= MAX_PREVIEWS
            && let Some(oldest) =
                oldest(self.previews.iter().map(|(hash, (_, used))| (hash, *used)))
        {
            let oldest = oldest.clone();
            if let Some((Some(image), _)) = self.previews.remove(&oldest) {
                self.dropped.push(image);
            }
        }
        let image = blurhash::decode(hash, PREVIEW_SIDE, PREVIEW_SIDE)
            .and_then(|pixels| RgbaImage::from_raw(PREVIEW_SIDE, PREVIEW_SIDE, pixels))
            .map(|mut pixels| {
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
            });
        self.previews
            .insert(hash.to_owned(), (image.clone(), self.clock));
        image
    }

    /// Previews no longer kept, for GPUI to drop.
    pub fn take_dropped(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.dropped)
    }

    /// Drops the least recently used pictures past [`MAX_PICTURES`].
    fn evict(&mut self) -> Vec<Arc<RenderImage>> {
        let mut dropped = Vec::new();
        while self.slots.len() > MAX_PICTURES {
            let Some(oldest) = oldest(self.slots.iter().map(|(key, (_, used))| (key, *used)))
            else {
                break;
            };
            let oldest = *oldest;
            if let Some((slot, _)) = self.slots.remove(&oldest) {
                dropped.extend(picture(slot));
            }
        }
        dropped
    }
}

fn picture(slot: Slot) -> Option<Arc<RenderImage>> {
    match slot {
        Slot::Ready(picture) => Some(picture.image),
        Slot::Loading | Slot::Failed => None,
    }
}

fn oldest<K>(entries: impl Iterator<Item = (K, u64)>) -> Option<K> {
    entries.min_by_key(|(_, used)| *used).map(|(key, _)| key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Slot {
        Slot::Ready(Picture::new(Decoded {
            frames: vec![Frame::new(RgbaImage::new(1, 1))],
            width: 1,
            height: 1,
        }))
    }

    fn id(n: i64) -> AttachmentId {
        AttachmentId::new(n)
    }

    #[test]
    fn pictures_load_once_per_use() {
        let mut pictures = Pictures::default();

        assert!(pictures.want(id(1), Use::Thumbnail));
        assert!(!pictures.want(id(1), Use::Thumbnail));
        assert!(pictures.want(id(1), Use::Full));
        assert_eq!(pictures.loading(id(1)), [Use::Thumbnail, Use::Full]);
        assert!(pictures.finish(id(1), Use::Thumbnail, ready()).is_empty());
        assert!(matches!(
            pictures.get(id(1), Use::Thumbnail),
            Some(Slot::Ready(_))
        ));
        assert_eq!(pictures.loading(id(1)), [Use::Full]);
        // No longer wanted: dropped at once.
        assert_eq!(pictures.finish(id(2), Use::Thumbnail, ready()).len(), 1);
        assert!(pictures.get(id(2), Use::Thumbnail).is_none());
    }

    #[test]
    fn the_least_recently_drawn_go_first() {
        let mut pictures = Pictures::default();
        for n in 0..MAX_PICTURES as i64 {
            pictures.want(id(n), Use::Thumbnail);
            pictures.finish(id(n), Use::Thumbnail, ready());
        }
        pictures.get(id(0), Use::Thumbnail);

        pictures.want(id(100), Use::Thumbnail);
        let dropped = pictures.finish(id(100), Use::Thumbnail, ready());

        assert_eq!(dropped.len(), 1);
        assert!(pictures.get(id(0), Use::Thumbnail).is_some());
        assert!(pictures.get(id(1), Use::Thumbnail).is_none());
    }

    #[test]
    fn forgetting_returns_pictures_to_drop() {
        let mut pictures = Pictures::default();
        pictures.want(id(1), Use::Thumbnail);
        pictures.finish(id(1), Use::Thumbnail, ready());
        pictures.want(id(1), Use::Full);
        pictures.want(id(2), Use::Full);
        pictures.finish(id(2), Use::Full, ready());

        assert_eq!(pictures.forget_full(Some(id(1))).len(), 1);
        assert!(pictures.get(id(2), Use::Full).is_none());
        assert!(pictures.get(id(1), Use::Full).is_some());
    }

    #[test]
    fn previews_are_cached_and_bad_hashes_give_none() {
        let mut pictures = Pictures::default();
        let first = pictures.preview("LEHV6nWB2yk8pyo0adR*.7kCMdnj").unwrap();
        let again = pictures.preview("LEHV6nWB2yk8pyo0adR*.7kCMdnj").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert!(pictures.preview("nope").is_none());
    }
}
