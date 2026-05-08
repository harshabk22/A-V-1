import re
with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Replace <action>...</action> in r#"<action>...</action>"#
text = re.sub(r'r#"<action>(.*?)</action>"#', r'r#"\1"#', text)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)

