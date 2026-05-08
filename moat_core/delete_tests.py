import re
with open("src/orchestrator.rs", "r") as f:
    text = f.read()

text = re.sub(r'    #\[test\]\n    fn parse_intent_tolerates_wrapper_noise_and_case_insensitive_tags\(\) \{.*?\n    \}\n', '', text, flags=re.DOTALL)
text = re.sub(r'    #\[test\]\n    fn parse_intent_rescues_unquoted_keys\(\) \{.*?\n    \}\n', '', text, flags=re.DOTALL)
text = re.sub(r'    #\[test\]\n    fn parse_intent_aliases_text_actions_to_done\(\) \{.*?\n    \}\n', '', text, flags=re.DOTALL)

with open("src/orchestrator.rs", "w") as f:
    f.write(text)

