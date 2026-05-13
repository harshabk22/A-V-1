use tiktoken_rs::cl100k_base;
use anyhow::Result;
use std::sync::OnceLock;

fn bpe_model() -> &'static tiktoken_rs::CoreBPE {
    static BPE: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| cl100k_base().expect("tiktoken cl100k_base model failed to load"))
}

pub struct TokenBudget {
    max_tokens: usize,
    current_tokens: usize,
    compression_threshold: f64,
}

impl TokenBudget {
    pub fn new_128k() -> Self {
        Self {
            max_tokens: 128_000,
            current_tokens: 0,
            compression_threshold: 0.70,
        }
    }

    pub fn count_tokens(text: &str) -> Result<usize> {
        let tokens = bpe_model().encode_with_special_tokens(text);
        Ok(tokens.len())
    }

    pub fn update(&mut self, text: &str) -> Result<()> {
        self.current_tokens = Self::count_tokens(text)?;
        Ok(())
    }

    pub fn status(&self) -> BudgetStatus {
        let usage_ratio = self.current_tokens as f64 / self.max_tokens as f64;

        if usage_ratio >= self.compression_threshold {
            BudgetStatus::NeedsCompression {
                current: self.current_tokens,
                max: self.max_tokens,
            }
        } else {
            BudgetStatus::Healthy {
                current: self.current_tokens,
                max: self.max_tokens,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    Healthy { current: usize, max: usize },
    NeedsCompression { current: usize, max: usize },
}