use crate::gateway::SessionSource;

/// A normalized message from any external channel.
/// All adapters convert platform-specific formats into this.
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub channel_id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub chat_type: ChatType,
    pub reply_to: Option<String>,
    pub media_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatType {
    DirectMessage,
    GroupChat,
    Channel,
    Thread,
}

/// Reply to send back to an external channel.
#[derive(Debug, Clone)]
pub struct ChannelReply {
    pub text: String,
    pub reply_to: Option<String>,
    pub media_urls: Vec<String>,
}

/// Trait for platform-specific channel adapters.
/// Paralyzes messages into ChannelMessage, formats replies back.
/// ZERO OS access — pure parsing and formatting only.
pub trait ChannelAdapter: Send + Sync {
    fn source(&self) -> SessionSource;
    fn name(&self) -> &'static str;
    fn max_chars(&self) -> usize;
    fn markdown_allowed(&self) -> bool;

    /// Parse a raw platform message into a normalized ChannelMessage.
    fn parse_message(&self, raw: &serde_json::Value) -> Option<ChannelMessage>;

    /// Format a reply for this platform.
    fn format_reply(&self, reply: &ChannelReply) -> String;

    /// Check if this adapter is healthy (e.g., auth valid, connection ok).
    /// Returns Ok(()) if healthy, Err(reason) if not.
    fn health_check(&self) -> Result<(), String> {
        Ok(()) // default: always healthy for stateless adapters
    }
}

// ── Telegram Adapter ──

pub struct TelegramAdapter;

impl ChannelAdapter for TelegramAdapter {
    fn source(&self) -> SessionSource { SessionSource::Telegram }
    fn name(&self) -> &'static str { "telegram" }
    fn max_chars(&self) -> usize { 4096 }
    fn markdown_allowed(&self) -> bool { true }

    fn parse_message(&self, raw: &serde_json::Value) -> Option<ChannelMessage> {
        let msg = raw.get("message")?;
        let chat = msg.get("chat")?;
        let from = msg.get("from")?;

        let chat_type = match chat.get("type")?.as_str()? {
            "private" => ChatType::DirectMessage,
            "group" | "supergroup" => ChatType::GroupChat,
            "channel" => ChatType::Channel,
            _ => ChatType::DirectMessage,
        };

        Some(ChannelMessage {
            channel_id: chat.get("id")?.to_string(),
            user_id: from.get("id")?.to_string(),
            user_name: from.get("first_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            text: msg.get("text")
                .or_else(|| msg.get("caption"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            chat_type,
            reply_to: msg.get("reply_to_message")
                .and_then(|r| r.get("message_id"))
                .map(|id| id.to_string()),
            media_urls: Vec::new(),
        })
    }

    fn format_reply(&self, reply: &ChannelReply) -> String {
        let mut text = reply.text.clone();
        if text.len() > self.max_chars() {
            text.truncate(self.max_chars() - 50);
            text.push_str("\n\n[truncated]");
        }
        if let Some(ref reply_id) = reply.reply_to {
            format!("{{\"reply_to_message_id\":{},\"text\":{}}}",
                reply_id,
                serde_json::to_string(&text).unwrap_or_default())
        } else {
            serde_json::to_string(&text).unwrap_or_default()
        }
    }
}

// ── WhatsApp Adapter ──

pub struct WhatsAppAdapter;

impl ChannelAdapter for WhatsAppAdapter {
    fn source(&self) -> SessionSource { SessionSource::WhatsApp }
    fn name(&self) -> &'static str { "whatsapp" }
    fn max_chars(&self) -> usize { 4096 }
    fn markdown_allowed(&self) -> bool { false }

    fn parse_message(&self, raw: &serde_json::Value) -> Option<ChannelMessage> {
        let entry = raw.get("entry")?.as_array()?.first()?;
        let changes = entry.get("changes")?.as_array()?.first()?;
        let value = changes.get("value")?;
        let messages = value.get("messages")?.as_array()?.first()?;

        Some(ChannelMessage {
            channel_id: value.get("metadata")?.get("phone_number_id")?.to_string(),
            user_id: messages.get("from")?.to_string(),
            user_name: value.get("contacts")?.as_array()?
                .first()?.get("profile")?.get("name")?
                .as_str().unwrap_or("unknown").to_string(),
            text: messages.get("text")?.get("body")?
                .as_str().unwrap_or("").to_string(),
            chat_type: ChatType::DirectMessage,
            reply_to: messages.get("context")?.get("id").map(|id| id.to_string()),
            media_urls: messages.get("image").and_then(|img| img.get("id"))
                .map(|id| vec![id.to_string()]).unwrap_or_default(),
        })
    }

    fn format_reply(&self, reply: &ChannelReply) -> String {
        let mut text = reply.text.clone();
        // WhatsApp: strip markdown, truncate
        text = text.replace("##", "").replace("**", "*").replace("`", "'");
        if text.len() > self.max_chars() {
            text.truncate(self.max_chars() - 50);
            text.push_str(" [truncated]");
        }
        let mut msg = serde_json::json!({
            "messaging_product": "whatsapp",
            "type": "text",
            "text": { "body": text }
        });
        if let Some(ref reply_id) = reply.reply_to {
            msg["context"] = serde_json::json!({ "message_id": reply_id });
        }
        serde_json::to_string(&msg).unwrap_or_default()
    }
}

// ── Discord Adapter ──

pub struct DiscordAdapter;

impl ChannelAdapter for DiscordAdapter {
    fn source(&self) -> SessionSource { SessionSource::Discord }
    fn name(&self) -> &'static str { "discord" }
    fn max_chars(&self) -> usize { 2000 }
    fn markdown_allowed(&self) -> bool { true }

    fn parse_message(&self, raw: &serde_json::Value) -> Option<ChannelMessage> {
        // Discord Gateway MESSAGE_CREATE event
        let d = raw.get("d")?;
        let author = d.get("author")?;

        let chat_type = if d.get("guild_id").is_some() {
            ChatType::GroupChat
        } else {
            ChatType::DirectMessage
        };

        Some(ChannelMessage {
            channel_id: d.get("channel_id")?.to_string(),
            user_id: author.get("id")?.to_string(),
            user_name: author.get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            text: d.get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            chat_type,
            reply_to: d.get("referenced_message")
                .and_then(|r| r.get("id"))
                .map(|id| id.to_string()),
            media_urls: d.get("attachments")
                .and_then(|a| a.as_array())
                .map(|arr| arr.iter()
                    .filter_map(|att| att.get("url").and_then(|u| u.as_str()).map(String::from))
                    .collect())
                .unwrap_or_default(),
        })
    }

    fn format_reply(&self, reply: &ChannelReply) -> String {
        let mut text = reply.text.clone();
        if text.len() > self.max_chars() {
            text.truncate(self.max_chars() - 50);
            text.push_str("\n\n[truncated]");
        }
        let mut msg = serde_json::json!({
            "content": text
        });
        if let Some(ref reply_id) = reply.reply_to {
            msg["message_reference"] = serde_json::json!({
                "message_id": reply_id
            });
        }
        serde_json::to_string(&msg).unwrap_or_default()
    }
}

// ── Channel Adapter Registry ──

pub struct AdapterRegistry {
    adapters: Vec<Box<dyn ChannelAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self { adapters: Vec::new() }
    }

    pub fn with_defaults() -> Self {
        let mut reg = Self::new();
        reg.register(Box::new(TelegramAdapter));
        reg.register(Box::new(WhatsAppAdapter));
        reg.register(Box::new(DiscordAdapter));
        reg
    }

    pub fn register(&mut self, adapter: Box<dyn ChannelAdapter>) {
        self.adapters.push(adapter);
    }

    pub fn get(&self, source: &SessionSource) -> Option<&Box<dyn ChannelAdapter>> {
        self.adapters.iter().find(|a| &a.source() == source)
    }

    pub fn sources(&self) -> Vec<SessionSource> {
        self.adapters.iter().map(|a| a.source()).collect()
    }

    pub fn count(&self) -> usize {
        self.adapters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telegram_parse_direct_message() {
        let adapter = TelegramAdapter;
        let raw = serde_json::json!({
            "message": {
                "message_id": 42,
                "from": { "id": 12345, "first_name": "Alice" },
                "chat": { "id": 67890, "type": "private" },
                "text": "Hello, AGEEX!"
            }
        });
        let msg = adapter.parse_message(&raw).expect("parse telegram msg");
        assert_eq!(msg.user_name, "Alice");
        assert_eq!(msg.text, "Hello, AGEEX!");
        assert_eq!(msg.chat_type, ChatType::DirectMessage);
        assert_eq!(msg.user_id, "12345");
    }

    #[test]
    fn test_telegram_format_reply() {
        let adapter = TelegramAdapter;
        let reply = ChannelReply {
            text: "Hi Alice!".into(),
            reply_to: Some("42".into()),
            media_urls: vec![],
        };
        let formatted = adapter.format_reply(&reply);
        assert!(formatted.contains("reply_to_message_id"));
        assert!(formatted.contains("Hi Alice!"));
    }

    #[test]
    fn test_whatsapp_strips_markdown() {
        let adapter = WhatsAppAdapter;
        assert!(!adapter.markdown_allowed());
        let reply = ChannelReply {
            text: "## Important\n**Bold** text".into(),
            reply_to: None,
            media_urls: vec![],
        };
        let formatted = adapter.format_reply(&reply);
        assert!(!formatted.contains("##"));
        assert!(!formatted.contains("**"));
        assert!(formatted.contains("Important"));
    }

    #[test]
    fn test_discord_max_chars_truncates() {
        let adapter = DiscordAdapter;
        assert_eq!(adapter.max_chars(), 2000);
        let long_text = "x".repeat(2500);
        let reply = ChannelReply {
            text: long_text,
            reply_to: None,
            media_urls: vec![],
        };
        let formatted = adapter.format_reply(&reply);
        assert!(formatted.len() < 2100);
        assert!(formatted.contains("[truncated]"));
    }

    #[test]
    fn test_registry_finds_adapters() {
        let reg = AdapterRegistry::with_defaults();
        assert_eq!(reg.count(), 3);
        assert!(reg.get(&SessionSource::Telegram).is_some());
        assert!(reg.get(&SessionSource::WhatsApp).is_some());
        assert!(reg.get(&SessionSource::Discord).is_some());
        assert!(reg.get(&SessionSource::DesktopUi).is_none());
    }
}
