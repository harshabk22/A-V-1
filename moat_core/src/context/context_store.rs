use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub timestamp: u64,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            timestamp: Self::now(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            timestamp: Self::now(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            timestamp: Self::now(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            timestamp: Self::now(),
        }
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

pub struct ContextStore {
    messages: Arc<Mutex<Vec<Message>>>,
    system_prompt: String,
}

impl ContextStore {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        let prompt = system_prompt.into();
        let messages = vec![Message::system(prompt.clone())];

        Self {
            messages: Arc::new(Mutex::new(messages)),
            system_prompt: prompt,
        }
    }

    pub fn append(&self, message: Message) -> Result<()> {
        let mut msgs = self.messages.lock().unwrap();
        msgs.push(message);
        Ok(())
    }

    pub fn get_all(&self) -> Vec<Message> {
        self.messages.lock().unwrap().clone()
    }

    pub fn replace_all(&self, messages: Vec<Message>) -> Result<()> {
        let mut msgs = self.messages.lock().unwrap();
        *msgs = messages;
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.messages.lock().unwrap().len()
    }

    pub fn prepend_system_addendum(&self, addendum: &str) -> Result<()> {
        let mut msgs = self.messages.lock().unwrap();
        if let Some(first) = msgs.first_mut() {
            if first.role == Role::System {
                first.content = format!("{}\n{}", first.content, addendum);
            }
        }
        Ok(())
    }
}