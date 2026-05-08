import re

with open("src/orchestrator.rs", "r") as f:
    code = f.read()

# 1. Update LlmMessage
old_llmmessage = """pub struct LlmMessage {
    pub role: String,
    pub content: String,
}"""
new_llmmessage = """pub struct LlmMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}"""
code = code.replace(old_llmmessage, new_llmmessage)

# 2. Update VersionedContext
old_vctx = """pub struct VersionedContext {
    history: Vec<String>,
    generation: AtomicU64,
}

impl VersionedContext {
    pub fn new(seed: Vec<String>) -> Self {
        Self {
            history: seed,
            generation: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.history.clone()
    }

    pub fn push(&mut self, entry: String) {
        self.history.push(entry);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }"""

new_vctx = """pub struct VersionedContext {
    history: Vec<LlmMessage>,
    generation: AtomicU64,
}

impl VersionedContext {
    pub fn new(seed: Vec<LlmMessage>) -> Self {
        Self {
            history: seed,
            generation: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> Vec<LlmMessage> {
        self.history.clone()
    }

    pub fn push(&mut self, entry: LlmMessage) {
        self.history.push(entry);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }"""
code = code.replace(old_vctx, new_vctx)

# 3. Update ReactContinuation
code = code.replace("pub context_trace: Vec<String>,", "pub context_trace: Vec<LlmMessage>,")

# 4. Update push_context and snapshot_context
code = code.replace("async fn push_context(context: &SharedContext, entry: String) {", "async fn push_context(context: &SharedContext, entry: LlmMessage) {")
code = code.replace("async fn snapshot_context(context: &SharedContext) -> Vec<String> {", "async fn snapshot_context(context: &SharedContext) -> Vec<LlmMessage> {")

# 5. Fix push_context calls
def replace_push_context(m):
    # m.group(1) is the arguments string like: &context_trace, format!("...")
    args = m.group(1)
    if "LlmMessage" in args: return m.group(0) # Already changed
    
    parts = args.split(",", 1)
    if len(parts) < 2: return m.group(0)
    ctx = parts[0].strip()
    msg = parts[1].strip()
    return f"push_context({ctx}, LlmMessage {{ role: \"system\".into(), content: Some({msg}), tool_calls: None, tool_call_id: None }})"

code = re.sub(r'push_context\((.*?)\)', replace_push_context, code, flags=re.DOTALL)

with open("src/orchestrator.rs", "w") as f:
    f.write(code)

print("Refactored orchestrator.rs structures.")
