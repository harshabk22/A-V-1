import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Fix mock provider
mock_old = """        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: usize,
            _temperature: f32,
            _tx: Option<tokio::sync::mpsc::Sender<String>>,
        ) -> Result<(String, u32), MoatError> {
            Ok((self.responses[self.index.fetch_add(1, Ordering::SeqCst) % self.responses.len()].clone(), 1))
        }"""
mock_new = """        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResponse, MoatError>> + Send + 'a>> {
            let resp = self.responses[self.index.fetch_add(1, Ordering::SeqCst) % self.responses.len()].clone();
            let intent: ActionIntent = serde_json::from_str(&resp).unwrap_or_else(|_| ActionIntent {
                thought: None,
                action: "done".into(),
                target: "user".into(),
                payload: Some(serde_json::Value::String(resp.clone()))
            });
            Box::pin(async move {
                Ok(LlmResponse {
                    content: None,
                    tool_calls: Some(vec![LlmToolCall {
                        id: "call_1".into(),
                        type_: "function".into(),
                        function: crate::orchestrator::LlmFunctionCall {
                            name: intent.action,
                            arguments: serde_json::json!({"target": intent.target, "payload": intent.payload}).to_string()
                        }
                    }]),
                    tokens: 1,
                })
            })
        }"""
text = text.replace(mock_old, mock_new)

# Remove the panicking chat_complete implementation
text = re.sub(r'fn chat_complete.*?Err\(MoatError::Llm\("chat_complete not implemented for this provider"\.into\(\)\).*?\}', '', text, flags=re.DOTALL)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
