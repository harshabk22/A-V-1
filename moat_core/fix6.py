import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Implement generate_intent_tools
old_generate = """    pub async fn generate_intent_tools(
        &self,
        model: &'a str,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
        max_tokens: usize,
        temperature: f32,
    ) -> MoatResult<LlmResponse> {
        unimplemented!()
    }"""

new_generate = """    pub async fn generate_intent_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
    ) -> MoatResult<LlmResponse> {
        let model = self.current_model();
        tokio::time::timeout(
            PLANNER_REPLY_TIMEOUT,
            self.provider.chat_complete(&model, messages, tools, self.config.max_tokens, self.config.temperature)
        ).await.map_err(|_| MoatError::Orchestrator("planner timeout".into()))?
    }
    
    pub fn build_llm_tools(&self) -> Vec<LlmTool> {
        vec![] // placeholder
    }"""
text = text.replace(old_generate, new_generate)

# Update the loop
loop_old = """                let prompt = snapshot_context(&context_trace).await.join("\\n\\n");
                let (raw, tokens) = match self
                    .planner
                    .generate_intent_raw_with_system(&cached_system_prompt, &prompt, status_tx.clone())
                    .await"""

loop_new = """                let mut messages = vec![LlmMessage { role: "system".into(), content: Some(cached_system_prompt.clone()), tool_calls: None, tool_call_id: None }];
                messages.extend(snapshot_context(&context_trace).await);
                let (raw, tokens) = match self.planner.generate_intent_tools(messages, None).await {
                    Ok(mut resp) => {
                        let mut action = "done".to_string();
                        let mut target = "user".to_string();
                        let mut payload = None;
                        if let Some(mut tc) = resp.tool_calls.take() {
                            if let Some(t) = tc.pop() {
                                action = t.function.name;
                                target = "user".to_string(); // fallback
                                if let Ok(args) = serde_json::from_str::<serde_json::Value>(&t.function.arguments) {
                                    if let Some(tgt) = args.get("target").and_then(|v| v.as_str()) {
                                        target = tgt.to_string();
                                    }
                                    if let Some(pld) = args.get("payload") {
                                        payload = Some(pld.to_string());
                                    }
                                }
                            }
                        }
                        let intent_json = serde_json::json!({
                            "action": action,
                            "target": target,
                            "payload": payload
                        }).to_string();
                        (intent_json, resp.tokens)
                    },"""
text = text.replace(loop_old, loop_new)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
