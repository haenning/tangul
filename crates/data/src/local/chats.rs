//! The assistant's conversations: a title and the messages, each kept as the JSON
//! the assistant wrote (passwords in SQL already redacted by it). Only the latest
//! ones are kept.

use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};

use super::Local;

/// Most conversations kept; older ones are dropped.
const KEEP: i64 = 200;

/// A conversation in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub id: i64,
    /// The database it was about, if any.
    pub database_id: Option<String>,
    pub title: String,
    /// Unix milliseconds of its last message.
    pub updated_at: i64,
}

impl Local {
    /// Starts a conversation; its id.
    pub fn create_chat(&self, database_id: Option<&str>, title: &str, at: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO chats (database_id, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![database_id, title, at],
        )?;
        let id = self.conn.last_insert_rowid();
        let old: Vec<i64> = self
            .conn
            .prepare_cached(
                "SELECT id FROM chats ORDER BY updated_at DESC, id DESC LIMIT -1 OFFSET ?1",
            )?
            .query_map(params![KEEP], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for chat in old {
            self.delete_chat(chat)?;
        }
        Ok(id)
    }

    /// Adds a message (JSON) at the end of conversation `chat`.
    pub fn add_chat_message(&self, chat: i64, message: &str, at: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO chat_messages (chat_id, seq, at, message)
             VALUES (?1, (SELECT coalesce(max(seq), 0) + 1 FROM chat_messages WHERE chat_id = ?1),
                     ?2, ?3)",
            params![chat, at, message],
        )?;
        tx.execute(
            "UPDATE chats SET updated_at = ?2 WHERE id = ?1",
            params![chat, at],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The latest conversations, newest first.
    pub fn chats(&self, limit: usize) -> Vec<ChatSummary> {
        let result = self
            .conn
            .prepare_cached(
                "SELECT id, database_id, title, updated_at FROM chats
                 ORDER BY updated_at DESC, id DESC LIMIT ?1",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![limit as i64], |r| {
                    Ok(ChatSummary {
                        id: r.get(0)?,
                        database_id: r.get(1)?,
                        title: r.get(2)?,
                        updated_at: r.get(3)?,
                    })
                })?
                .collect()
            });
        result.unwrap_or_else(|e| {
            eprintln!("chats query failed: {e}");
            Vec::new()
        })
    }

    pub fn chat(&self, chat: i64) -> Option<ChatSummary> {
        self.conn
            .query_row(
                "SELECT id, database_id, title, updated_at FROM chats WHERE id = ?1",
                params![chat],
                |r| {
                    Ok(ChatSummary {
                        id: r.get(0)?,
                        database_id: r.get(1)?,
                        title: r.get(2)?,
                        updated_at: r.get(3)?,
                    })
                },
            )
            .optional()
            .unwrap_or_else(|e| {
                eprintln!("chat query failed: {e}");
                None
            })
    }

    /// The messages (JSON) of conversation `chat`, in order.
    pub fn chat_messages(&self, chat: i64) -> Vec<String> {
        let result = self
            .conn
            .prepare_cached("SELECT message FROM chat_messages WHERE chat_id = ?1 ORDER BY seq")
            .and_then(|mut stmt| stmt.query_map(params![chat], |r| r.get(0))?.collect());
        result.unwrap_or_else(|e| {
            eprintln!("chat messages query failed: {e}");
            Vec::new()
        })
    }

    pub fn rename_chat(&self, chat: i64, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET title = ?2 WHERE id = ?1",
            params![chat, title],
        )?;
        Ok(())
    }

    pub fn delete_chat(&self, chat: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM chat_messages WHERE chat_id = ?1",
            params![chat],
        )?;
        tx.execute("DELETE FROM chats WHERE id = ?1", params![chat])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_keep_their_messages_in_order() {
        let l = Local::in_memory();
        let a = l.create_chat(Some("db"), "Orders?", 10).unwrap();
        let b = l.create_chat(None, "Hello", 20).unwrap();
        l.add_chat_message(a, "{\"n\":1}", 30).unwrap();
        l.add_chat_message(a, "{\"n\":2}", 31).unwrap();
        assert_eq!(l.chat_messages(a), ["{\"n\":1}", "{\"n\":2}"]);
        assert_eq!(l.chat_messages(b), Vec::<String>::new());
        // The one with the latest message first.
        let list = l.chats(10);
        assert_eq!(list.iter().map(|c| c.id).collect::<Vec<_>>(), [a, b]);
        assert_eq!(list[0].database_id.as_deref(), Some("db"));
        assert_eq!(list[0].updated_at, 31);
        l.rename_chat(b, "Greeting").unwrap();
        assert_eq!(l.chat(b).unwrap().title, "Greeting");
        l.delete_chat(a).unwrap();
        assert_eq!(l.chat(a), None);
        assert!(l.chat_messages(a).is_empty());
        assert_eq!(l.chats(10).len(), 1);
    }

    #[test]
    fn only_the_latest_are_kept() {
        let l = Local::in_memory();
        let first = l.create_chat(None, "0", 0).unwrap();
        l.add_chat_message(first, "{}", 0).unwrap();
        for i in 1..=KEEP {
            l.create_chat(None, &i.to_string(), i).unwrap();
        }
        assert_eq!(l.chats(1000).len(), KEEP as usize);
        assert_eq!(l.chat(first), None);
        assert!(l.chat_messages(first).is_empty());
    }
}
