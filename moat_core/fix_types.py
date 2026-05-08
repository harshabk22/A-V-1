import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# 1. replacement.push(format!(...))
text = text.replace(
    'replacement.push(format!("[FACTS FROM PRIOR TURNS]\\n{}", facts));',
    'replacement.push(LlmMessage { role: "system".into(), content: Some(format!("[FACTS FROM PRIOR TURNS]\\n{}", facts)), tool_calls: None, tool_call_id: None });'
)

text = text.replace(
    'replacement.push(format!("[SUMMARY OF DROPPED TURNS]\\n{}", summary));',
    'replacement.push(LlmMessage { role: "system".into(), content: Some(format!("[SUMMARY OF DROPPED TURNS]\\n{}", summary)), tool_calls: None, tool_call_id: None });'
)

text = text.replace(
    'fn try_swap(&mut self, expected_generation: u64, replacement: Vec<String>)',
    'fn try_swap(&mut self, expected_generation: u64, replacement: Vec<LlmMessage>)'
)

# 2. entry.contains -> entry.content.as_deref().unwrap_or("").contains
text = text.replace(
    '.filter(|entry| entry.contains("SUCCESS:") || entry.contains("FAILED:"))',
    '.filter(|entry| entry.content.as_deref().unwrap_or("").contains("SUCCESS:") || entry.content.as_deref().unwrap_or("").contains("FAILED:"))'
)

# 3. .join("\n") and chars() in prune_context_if_needed
text = text.replace(
    '.map(|entry| entry.chars().take(240).collect::<String>())',
    '.map(|entry| entry.content.as_deref().unwrap_or("").chars().take(240).collect::<String>())'
)

text = text.replace(
    '.collect::<Vec<_>>()\n            .join("\\n")',
    '.map(|m| m.content.clone().unwrap_or_default())\n            .collect::<Vec<_>>()\n            .join("\\n")'
)

# 4. vec![context_bootstrap.to_string()]
text = text.replace(
    'vec![context_bootstrap.to_string()]',
    'vec![LlmMessage { role: "system".into(), content: Some(context_bootstrap.to_string()), tool_calls: None, tool_call_id: None }]'
)

# 5. Active memory prelude push
text = text.replace(
    'initial_trace.push(format!(\n                        "<active_memory_prelude>\\n{}\\n</active_memory_prelude>",\n                        prelude.context_block\n                    ));',
    'initial_trace.push(LlmMessage { role: "system".into(), content: Some(format!(\n                        "<active_memory_prelude>\\n{}\\n</active_memory_prelude>",\n                        prelude.context_block\n                    )), tool_calls: None, tool_call_id: None });'
)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
