import re

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

bad_match = """                    },
                {
                    Ok(result) => result,"""
good_match = """                    },"""

text = text.replace(bad_match, good_match)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
