import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Replace trailing `}))` or `}))\n` with `})`
text = re.sub(r'tool_call_id: None \}\)\)', r'tool_call_id: None })', text)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
