//! Small, terminal-safe history of server chat and game-event messages.

use std::collections::VecDeque;

const MAX_MESSAGES: usize = 64;
const MAX_MESSAGE_CHARACTERS: usize = 512;

/// A bounded history of chat and system messages received from the server.
///
/// The live client owns writes and the terminal renderer reads cloned recent
/// lines. Keeping this independent of Azalea's ECS prevents terminal drawing
/// from holding a game-state lock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatFeed {
    messages: VecDeque<String>,
}

impl ChatFeed {
    /// Add one server-supplied message after removing terminal control codes.
    pub fn push(&mut self, message: impl AsRef<str>) {
        let message = terminal_safe_text(message.as_ref());
        if message.is_empty() {
            return;
        }
        if self.messages.len() == MAX_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
    }

    /// Return at most `limit` newest messages in display order.
    pub fn recent(&self, limit: usize) -> Vec<String> {
        let start = self.messages.len().saturating_sub(limit);
        self.messages.iter().skip(start).cloned().collect()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

fn terminal_safe_text(message: &str) -> String {
    message
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            character if character.is_control() => '�',
            character => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_MESSAGE_CHARACTERS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_keeps_recent_messages_in_display_order() {
        let mut feed = ChatFeed::default();
        feed.push("first");
        feed.push("second");
        feed.push("third");

        assert_eq!(feed.recent(2), ["second", "third"]);
    }

    #[test]
    fn feed_sanitizes_terminal_controls_and_whitespace() {
        let mut feed = ChatFeed::default();
        feed.push(" hello\n\x1b[2J\tworld ");

        assert_eq!(feed.recent(1), ["hello �[2J world"]);
    }
}
