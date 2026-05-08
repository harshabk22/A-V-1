import json

next_0 = r#"{"action":"memory:learn","target":"procedural","payload":{"keywords":"github api curl token","skill_description":"To authenticate with GitHub API, send header Authorization: Bearer <token>."}}"#
cleaned = next_0.replace("<action>", "").replace("</action>", "").strip()

try:
    data = json.loads(cleaned)
    print("Parsed ok!")
    print(type(data["payload"]))
except Exception as e:
    print(f"Error: {e}")
