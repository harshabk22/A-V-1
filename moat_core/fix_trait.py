import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

trait_start = """pub trait LlmProvider: Send + Sync {
    #[deprecated(note = "Migrating to strictly typed tool_calls via chat_complete")]
    fn complete<'a>("""

new_trait = """pub trait LlmProvider: Send + Sync {
    fn chat_complete<'a>(
        &'a self,
        model: &'a str,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
        max_tokens: usize,
        temperature: f32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResponse, MoatError>> + Send + 'a>> {
        Box::pin(async move {
            Err(MoatError::Llm("chat_complete not implemented for this provider".into()))
        })
    }

    #[deprecated(note = "Migrating to strictly typed tool_calls via chat_complete")]
    fn complete<'a>("""

text = text.replace(trait_start, new_trait)

# Also fix the `}` that was left behind
text = text.replace("    )\n    }\n", "    )\n")

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
