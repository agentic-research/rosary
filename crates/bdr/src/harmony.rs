// Harmony encoding extensions for BDR

use openai_harmony::chat::{Message, Role, SystemContent};

use crate::channels::BdrChannel;

/// Build a BDR system message with decade/thread/bead channels configured.
pub fn bdr_system_content() -> SystemContent {
    SystemContent::new()
        .with_model_identity("BDR Lattice Decomposition Agent")
        .with_required_channels(["decade", "thread", "bead"])
}

/// Build a decade-channel message (ADR-level rationale).
pub fn decade_message(content: &str) -> Message {
    Message::from_role_and_content(Role::Assistant, content).with_channel("decade")
}

/// Build a thread-channel message with cross-repo routing.
pub fn thread_message(content: &str, recipient: &str) -> Message {
    Message::from_role_and_content(Role::Assistant, content)
        .with_channel("thread")
        .with_recipient(recipient)
}

/// Build a bead-channel message (atomic deliverable).
pub fn bead_message(content: &str) -> Message {
    Message::from_role_and_content(Role::Assistant, content).with_channel("bead")
}

/// Extract BDR channel from a Message, returns None if no channel or unknown channel.
pub fn message_channel(msg: &Message) -> Option<BdrChannel> {
    msg.channel
        .as_deref()
        .and_then(|ch| BdrChannel::parse(ch).ok())
}

/// Filter messages by BDR channel.
pub fn messages_by_channel(messages: &[Message], channel: BdrChannel) -> Vec<&Message> {
    messages
        .iter()
        .filter(|m| message_channel(m) == Some(channel))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openai_harmony::chat::Content;

    #[test]
    fn system_content_has_bdr_channels() {
        let sys = bdr_system_content();
        let cfg = sys.channel_config.unwrap();
        assert!(cfg.channel_required);
        assert_eq!(cfg.valid_channels, vec!["decade", "thread", "bead"]);
    }

    #[test]
    fn system_content_has_identity() {
        let sys = bdr_system_content();
        assert_eq!(
            sys.model_identity.unwrap(),
            "BDR Lattice Decomposition Agent"
        );
    }

    #[test]
    fn decade_message_fields() {
        let msg = decade_message("design rationale here");
        assert_eq!(msg.channel.as_deref(), Some("decade"));
        assert_eq!(msg.author.role, Role::Assistant);
        assert!(msg.recipient.is_none());
        match &msg.content[0] {
            Content::Text(t) => assert_eq!(t.text, "design rationale here"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn thread_message_has_recipient() {
        let msg = thread_message("implement auth", "mache:bead-85t");
        assert_eq!(msg.channel.as_deref(), Some("thread"));
        assert_eq!(msg.recipient.as_deref(), Some("mache:bead-85t"));
        assert_eq!(msg.author.role, Role::Assistant);
    }

    #[test]
    fn bead_message_fields() {
        let msg = bead_message("PR merged");
        assert_eq!(msg.channel.as_deref(), Some("bead"));
        assert!(msg.recipient.is_none());
    }

    #[test]
    fn message_channel_extracts_correctly() {
        assert_eq!(
            message_channel(&decade_message("x")),
            Some(BdrChannel::Decade)
        );
        assert_eq!(
            message_channel(&thread_message("x", "y")),
            Some(BdrChannel::Thread)
        );
        assert_eq!(message_channel(&bead_message("x")), Some(BdrChannel::Bead));
    }

    #[test]
    fn message_channel_none_for_no_channel() {
        let msg = Message::from_role_and_content(Role::User, "hello");
        assert_eq!(message_channel(&msg), None);
    }

    #[test]
    fn messages_by_channel_filters() {
        let msgs = vec![
            decade_message("rationale"),
            thread_message("impl", "repo"),
            bead_message("done"),
            decade_message("more rationale"),
        ];
        let decades = messages_by_channel(&msgs, BdrChannel::Decade);
        assert_eq!(decades.len(), 2);
        let beads = messages_by_channel(&msgs, BdrChannel::Bead);
        assert_eq!(beads.len(), 1);
    }

    #[test]
    fn message_serde_roundtrip() {
        let msg = thread_message("cross-repo work", "rosary:rsry-99c096");
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel.as_deref(), Some("thread"));
        assert_eq!(back.recipient.as_deref(), Some("rosary:rsry-99c096"));
    }
}
