//! Virtual channel clipboard synchronization for Perigee sessions; file transfer will come from `simply-transfer-link`.

pub mod clipboard;

pub use clipboard::*;

use anyhow::Result;

use perigee_protocol::VirtualChannelMessage;

/// Unified Orchestration Manager for all Virtual Channels
#[derive(Default)]
pub struct VirtualChannelManager {
    pub clipboard_handler: ClipboardChannelHandler,
}

impl VirtualChannelManager {
    pub fn new() -> Self {
        Self {
            clipboard_handler: ClipboardChannelHandler::new(),
        }
    }

    /// Route incoming virtual channel messages to specialized handlers
    pub async fn route_channel_message(&self, msg: VirtualChannelMessage) -> Result<()> {
        match msg {
            VirtualChannelMessage::Clipboard(payload) => {
                self.clipboard_handler
                    .handle_remote_clipboard(payload)
                    .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perigee_protocol::{
        ClipboardPayload, WireMessage, decode_length_prefixed, encode_length_prefixed,
    };

    #[tokio::test]
    async fn test_route_channel_message_all_variants() {
        let manager = VirtualChannelManager::new();

        // 1. VirtualChannelMessage::Clipboard (Text, Image, and Files payloads)
        manager
            .route_channel_message(VirtualChannelMessage::Clipboard(ClipboardPayload::Text(
                "Perigee virtual channel clipboard test".to_string(),
            )))
            .await
            .expect("Routing ClipboardPayload::Text should succeed");
        assert_eq!(
            manager.clipboard_handler.last_content().await,
            ClipboardContent::Text("Perigee virtual channel clipboard test".to_string())
        );

        let img_bytes = vec![255u8; 16];
        manager
            .route_channel_message(VirtualChannelMessage::Clipboard(ClipboardPayload::Image {
                width: 2,
                height: 2,
                format: "raw/rgba8".to_string(),
                data: img_bytes.clone(),
            }))
            .await
            .expect("Routing ClipboardPayload::Image should succeed");
        assert_eq!(
            manager.clipboard_handler.last_content().await,
            ClipboardContent::ImageHash(calculate_image_hash(&img_bytes))
        );

        manager
            .route_channel_message(VirtualChannelMessage::Clipboard(ClipboardPayload::Files(
                vec!["/tmp/perigee-test/report.pdf".to_string()],
            )))
            .await
            .expect("Routing ClipboardPayload::Files should succeed");
        assert_eq!(
            manager.clipboard_handler.last_content().await,
            ClipboardContent::Files(vec!["/tmp/perigee-test/report.pdf".to_string()])
        );
    }

    #[tokio::test]
    async fn test_route_channel_message_postcard_wire_roundtrip() {
        let manager = VirtualChannelManager::new();

        let wire_frames = vec![
            WireMessage::Channel(VirtualChannelMessage::Clipboard(ClipboardPayload::Text(
                "postcard-framed-clipboard-text".to_string(),
            ))),
            WireMessage::Channel(VirtualChannelMessage::Clipboard(ClipboardPayload::Image {
                width: 4,
                height: 4,
                format: "raw/rgba8".to_string(),
                data: vec![128u8; 64],
            })),
            WireMessage::Channel(VirtualChannelMessage::Clipboard(ClipboardPayload::Files(
                vec![
                    "/tmp/perigee-test/alpha.tar.zst".to_string(),
                    "/tmp/perigee-test/beta.sig".to_string(),
                ],
            ))),
        ];

        for frame in wire_frames {
            let encoded = encode_length_prefixed(&frame)
                .expect("Length-prefixed postcard encoding should succeed");
            let decoded = decode_length_prefixed(&encoded)
                .expect("Length-prefixed postcard decoding should succeed");
            assert_eq!(decoded, frame);

            let WireMessage::Channel(channel_msg) = decoded else {
                panic!("Expected WireMessage::Channel");
            };
            manager
                .route_channel_message(channel_msg)
                .await
                .expect("Routing decoded VirtualChannelMessage should succeed");
        }
    }
}
