import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Fix memory:search budget line
bad_line1 = 'push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!("Turn {} OBSERVATION: memory:search budget (10)), tool_calls: None, tool_call_id: None }) exhausted. You must proceed with available context.", turns))), tool_calls: None, tool_call_id: None }).await;'
good_line1 = 'push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!("Turn {} OBSERVATION: memory:search budget (10) exhausted. You must proceed with available context.", turns)), tool_calls: None, tool_call_id: None }).await;'
text = text.replace(bad_line1, good_line1)

# Fix session:search budget line
bad_line2 = 'push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!("Turn {} OBSERVATION: session:search budget (6)), tool_calls: None, tool_call_id: None }) exhausted. Continue with the transcripts already retrieved.", turns))), tool_calls: None, tool_call_id: None }).await;'
good_line2 = 'push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!("Turn {} OBSERVATION: session:search budget (6) exhausted. Continue with the transcripts already retrieved.", turns)), tool_calls: None, tool_call_id: None }).await;'
text = text.replace(bad_line2, good_line2)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
