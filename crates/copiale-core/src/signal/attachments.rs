//! Downloading attachments from Signal's CDN, and describing ours for
//! upload.
//!
//! Downloads are like presage's `Manager::get_attachment`, but read no more
//! than the attachment's declared size allows.

use futures::AsyncReadExt as _;
use presage::libsignal_service::attachment_cipher::decrypt_in_place;
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::libsignal_service::proto::AttachmentPointer;
use presage::libsignal_service::push_service::PushService;
use presage::libsignal_service::sender::AttachmentSpec;
use sha2::{Digest as _, Sha256};

use super::errors::cdn_error;
use crate::backend::BackendError;
use crate::model::{Attachment, MAX_ATTACHMENT_SIZE, RemoteAttachment};

/// presage's, which its other requests send.
const USER_AGENT: &str = "presage-rs-0.8.0-dev";

/// IV, a block of PKCS#7 padding, and the HMAC around the padded file.
const OVERHEAD: u64 = 16 + 16 + 32;

/// Downloads and decrypts `attachment`. CDN requests carry no credentials.
pub(super) async fn download(
    servers: SignalServers,
    attachment: RemoteAttachment,
) -> Result<Vec<u8>, BackendError> {
    let pointer = pointer(&attachment)?;
    let size = pointer
        .size
        .filter(|size| *size <= MAX_ATTACHMENT_SIZE)
        .ok_or_else(|| BackendError::new("attachment too large"))?;
    let mut service = PushService::new(servers, None, USER_AGENT);
    let download = service.get_attachment(&pointer).await.map_err(cdn_error)?;
    let limit = max_encrypted_size(size);
    let mut ciphertext = Vec::new();
    download
        .stream
        .take(limit + 1)
        .read_to_end(&mut ciphertext)
        .await
        .map_err(|error| BackendError::network(error.to_string()))?;
    if ciphertext.len() as u64 > limit {
        return Err(BackendError::new("attachment larger than it says"));
    }
    tokio::task::spawn_blocking(move || decrypt(&pointer, ciphertext))
        .await
        .map_err(|_| BackendError::new("decrypting the attachment failed"))?
}

/// What recipients learn of `attachment` besides its contents.
pub(super) fn spec(attachment: Attachment, length: usize) -> AttachmentSpec {
    AttachmentSpec {
        content_type: attachment
            .content_type
            .unwrap_or_else(|| "application/octet-stream".into()),
        length,
        file_name: attachment.name,
        preview: None,
        voice_note: None,
        borderless: None,
        width: attachment.width,
        height: attachment.height,
        caption: None,
        blur_hash: attachment.blurhash,
    }
}

/// `attachment` as sent in a message.
pub(super) fn pointer(attachment: &RemoteAttachment) -> Result<AttachmentPointer, BackendError> {
    AttachmentPointer::decode(attachment.as_bytes())
        .map_err(|error| BackendError::new(format!("invalid attachment: {error}")))
}

/// Checks the downloaded file against the pointer's digest and decrypts it.
fn decrypt(pointer: &AttachmentPointer, mut ciphertext: Vec<u8>) -> Result<Vec<u8>, BackendError> {
    let invalid = || BackendError::new("attachment corrupt");
    let digest = pointer.digest.as_deref().ok_or_else(invalid)?;
    if ciphertext.len() < OVERHEAD as usize || Sha256::digest(&ciphertext)[..] != *digest {
        return Err(invalid());
    }
    let key: [u8; 64] = pointer.key().try_into().map_err(|_| invalid())?;
    decrypt_in_place(key, &mut ciphertext).map_err(|_| invalid())?;
    // Senders pad the file before encrypting it.
    ciphertext.truncate(pointer.size.unwrap_or(0) as usize);
    Ok(ciphertext)
}

/// The most an attachment of `size` bytes takes on the CDN: Signal's
/// clients pad files to at least 541 bytes, then to the next power of
/// 1.05; a little more is allowed for rounding.
fn max_encrypted_size(size: u32) -> u64 {
    let size = f64::from(size.max(1));
    let padded = 1.05f64.powf(size.log(1.05).ceil()).max(541.0);
    (padded * 1.01) as u64 + OVERHEAD
}

#[cfg(test)]
mod tests {
    use presage::libsignal_service::attachment_cipher::encrypt_in_place;

    use super::*;

    /// A pointer to `contents` encrypted, and the encrypted file, padded
    /// as Signal's clients pad.
    fn encrypted(contents: &[u8]) -> (AttachmentPointer, Vec<u8>) {
        let key = [7u8; 64];
        let mut file = contents.to_vec();
        let padded = 1.05f64
            .powf((contents.len().max(1) as f64).log(1.05).ceil())
            .max(541.0) as usize;
        file.resize(padded, 0);
        encrypt_in_place([3; 16], key, &mut file);
        let pointer = AttachmentPointer {
            key: Some(key.to_vec()),
            size: Some(contents.len() as u32),
            digest: Some(Sha256::digest(&file).to_vec()),
            ..Default::default()
        };
        (pointer, file)
    }

    #[test]
    fn files_decrypt_without_their_padding() {
        let contents = b"picture".repeat(1000);
        let (pointer, file) = encrypted(&contents);

        assert!(file.len() as u64 <= max_encrypted_size(contents.len() as u32));
        assert_eq!(decrypt(&pointer, file).unwrap(), contents);
    }

    #[test]
    fn padded_sizes_stay_within_the_limit() {
        for size in [0, 1, 540, 541, 542, 10_000, 1_234_567, MAX_ATTACHMENT_SIZE] {
            let padded = 1.05f64
                .powf(f64::from(size.max(1)).log(1.05).ceil())
                .max(541.0) as u64;
            let encrypted = (padded / 16 + 1) * 16 + 16 + 32;
            assert!(encrypted <= max_encrypted_size(size), "{size}");
        }
        assert!(max_encrypted_size(1000) < 2000);
    }

    #[test]
    fn tampered_or_short_files_are_refused() {
        let (pointer, mut file) = encrypted(b"hello");
        file[20] ^= 1;
        assert!(decrypt(&pointer, file).is_err());

        let (mut pointer, file) = encrypted(b"hello");
        pointer.digest = Some(Sha256::digest(b"short").to_vec());
        assert!(decrypt(&pointer, b"short".to_vec()).is_err());
        pointer.digest = None;
        assert!(decrypt(&pointer, file).is_err());
    }
}
