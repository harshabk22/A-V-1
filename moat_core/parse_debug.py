import re
with open("src/orchestrator.rs", "r") as f:
    text = f.read()

text = text.replace(
    'let intent: ActionIntent = serde_json::from_str(&cleaned).unwrap_or_else(|_| ActionIntent {',
    'let intent: ActionIntent = serde_json::from_str(&cleaned).unwrap_or_else(|e| { println!("PARSE ERROR: {}", e); ActionIntent {'
)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)

