import { useState, useEffect, useRef } from "react";

interface CommandPaletteProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (command: CommandOption) => void;
}

export interface CommandOption {
  id: string;
  label: string;
  description: string;
  icon: string;
  action: () => void;
}

const BUILTIN_COMMANDS: CommandOption[] = [
  {
    id: "switch-agent1",
    label: "Switch to Agent1 (Read)",
    description: "Spotlight read-only assistant",
    icon: "🔍",
    action: () => {},
  },
  {
    id: "switch-agent2",
    label: "Switch to Agent2 (Execute)",
    description: "Full-capability executor agent",
    icon: "⚡",
    action: () => {},
  },
  {
    id: "switch-agent3",
    label: "Switch to AGX3 (Connect)",
    description: "External channel connector agent",
    icon: "🔗",
    action: () => {},
  },
  {
    id: "open-settings",
    label: "Open Settings",
    description: "Model, API keys, routing configuration",
    icon: "⚙️",
    action: () => {},
  },
  {
    id: "toggle-consent",
    label: "View Consent Log",
    description: "Review approval decisions and grants",
    icon: "🛡️",
    action: () => {},
  },
  {
    id: "new-session",
    label: "New Session",
    description: "Start a fresh agent conversation",
    icon: "➕",
    action: () => {},
  },
];

export function CommandPalette({ isOpen, onClose, onSelect }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const filtered = BUILTIN_COMMANDS.filter(
    (cmd) =>
      cmd.label.toLowerCase().includes(query.toLowerCase()) ||
      cmd.description.toLowerCase().includes(query.toLowerCase())
  );

  useEffect(() => {
    if (isOpen) {
      setQuery("");
      setSelectedIndex(0);
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [isOpen]);

  useEffect(() => {
    setSelectedIndex(0);
  }, [query]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((i) => Math.min(i + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter" && filtered[selectedIndex]) {
      onSelect(filtered[selectedIndex]);
      onClose();
    }
  };

  if (!isOpen) return null;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 9999,
        display: "flex",
        justifyContent: "center",
        paddingTop: "15vh",
        backgroundColor: "rgba(0,0,0,0.3)",
      }}
      onClick={onClose}
    >
      <div
        style={{
          width: 560,
          maxHeight: 400,
          backgroundColor: "rgba(30,30,35,0.95)",
          borderRadius: 12,
          border: "1px solid rgba(255,255,255,0.12)",
          boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
          overflow: "hidden",
          backdropFilter: "blur(20px)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          type="text"
          placeholder="Search commands..."
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={handleKeyDown}
          style={{
            width: "100%",
            padding: "14px 18px",
            fontSize: 16,
            border: "none",
            outline: "none",
            backgroundColor: "transparent",
            color: "#fff",
            borderBottom: "1px solid rgba(255,255,255,0.08)",
          }}
          autoFocus
        />
        <div style={{ maxHeight: 340, overflowY: "auto" }}>
          {filtered.map((cmd, i) => (
            <div
              key={cmd.id}
              onClick={() => {
                onSelect(cmd);
                onClose();
              }}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 12,
                padding: "10px 18px",
                cursor: "pointer",
                backgroundColor:
                  i === selectedIndex
                    ? "rgba(100,130,255,0.2)"
                    : "transparent",
                borderLeft:
                  i === selectedIndex
                    ? "3px solid rgba(100,130,255,0.8)"
                    : "3px solid transparent",
                transition: "background-color 0.1s",
              }}
            >
              <span style={{ fontSize: 18 }}>{cmd.icon}</span>
              <div>
                <div style={{ color: "#fff", fontSize: 14, fontWeight: 500 }}>
                  {cmd.label}
                </div>
                <div style={{ color: "rgba(255,255,255,0.5)", fontSize: 12 }}>
                  {cmd.description}
                </div>
              </div>
            </div>
          ))}
          {filtered.length === 0 && (
            <div
              style={{
                padding: 20,
                textAlign: "center",
                color: "rgba(255,255,255,0.4)",
              }}
            >
              No commands found
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
