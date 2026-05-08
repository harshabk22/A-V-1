import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

stub_provider_impl = """    impl LlmProvider for StubProvider {
        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<LlmResponse>> {
            Box::pin(async move {
                let next = self
                    .responses
                    .lock()
                    .expect("lock stub responses")
                    .pop_front()
                    .expect("stub response");
                
                let intent: ActionIntent = serde_json::from_str(&next.0).unwrap_or_else(|_| ActionIntent {
                    thought: None,
                    action: "done".into(),
                    target: "user".into(),
                    payload: Some(serde_json::Value::String(next.0.clone()))
                });
                
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
                    tokens: next.1,
                })
            })
        }

        fn complete<'a>("""

text = text.replace("    impl LlmProvider for StubProvider {\n        fn complete<'a>(", stub_provider_impl)

hanging_provider_impl = """    impl LlmProvider for HangingProvider {
        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<LlmResponse>> {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_secs(31)).await;
                Ok(LlmResponse { content: Some("late".into()), tool_calls: None, tokens: 1 })
            })
        }

        fn complete<'a>("""

text = text.replace("    impl LlmProvider for HangingProvider {\n        fn complete<'a>(", hanging_provider_impl)

hanging_planner_impl = """    impl LlmProvider for HangingPlannerProvider {
        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<LlmResponse>> {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(LlmResponse {
                    content: None,
                    tool_calls: Some(vec![LlmToolCall {
                        id: "call_2".into(),
                        type_: "function".into(),
                        function: crate::orchestrator::LlmFunctionCall {
                            name: "done".into(),
                            arguments: serde_json::json!({"target": "user", "payload": "late"}).to_string()
                        }
                    }]),
                    tokens: 1
                })
            })
        }

        fn complete<'a>("""

text = text.replace("    impl LlmProvider for HangingPlannerProvider {\n        fn complete<'a>(", hanging_planner_impl)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
