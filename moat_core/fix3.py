import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Pattern: tool_call_id: None }), followed by whitespace and a single )
text = re.sub(r'tool_call_id: None \}\),\s*\)', r'tool_call_id: None } )', text)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
