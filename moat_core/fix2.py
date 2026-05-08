import sys

with open("src/orchestrator.rs", "r") as f:
    text = f.read()

# Fix the broken ones.
# 1. format!("...", turns), tool_calls: None
text = text.replace('", turns, err), tool_calls: None', '", turns, err)), tool_calls: None')
text = text.replace('", turns, observation), tool_calls: None', '", turns, observation)), tool_calls: None')
text = text.replace('", turns, message), tool_calls: None', '", turns, message)), tool_calls: None')
text = text.replace('", turns)).await;', '", turns))), tool_calls: None, tool_call_id: None }).await;')
text = text.replace('}, tool_calls: None, tool_call_id: None }) exhausted. You must proceed with available context.", turns)).await;', 'exhausted. You must proceed with available context.", turns))), tool_calls: None, tool_call_id: None }).await;')
text = text.replace('}, tool_calls: None, tool_call_id: None }) exhausted. Continue with the transcripts already retrieved.", turns)).await;', 'exhausted. Continue with the transcripts already retrieved.", turns))), tool_calls: None, tool_call_id: None }).await;')

# Now fix the multi-line format!( that were truncated
# We search for `content: Some(format!(` and match the next `tool_calls: None`
import re
def replacer(m):
    # m.group(0) is the entire match from `Some(format!(` to `tool_calls: None`
    # We want to insert `)` before `, tool_calls: None` if it's missing.
    # The original was `format!(... )` and got cut. The matching closing paren for `format!` was lost.
    # So we replace `, tool_calls: None` with `), tool_calls: None`
    s = m.group(0)
    # count parens inside
    open_p = s.count('(')
    close_p = s.count(')')
    if open_p > close_p:
        # add the missing )
        return s[:-16] + ")" + s[-16:] # `, tool_calls: N`
    return s

text = re.sub(r'Some\(format!\(.*?tool_calls: None', replacer, text, flags=re.DOTALL)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)
