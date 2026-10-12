import { useEffect, useRef } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../stores/chatStore";
import ReasoningPanel from "./ReasoningPanel";
import {
  type GatewaySessionContext,
  type MemoryOverview,
  type GatewaySessionRecord,
} from "../types/ipc";

function MessageBody({
  text,
  isStreaming,
  role,
}: {
  text: string;
  isStreaming?: boolean;
  role: "user" | "agent" | "system";
}) {
  const accent =
    role === "system" ? "#ff9fb2" : role === "user" ? "#9dc0ff" : "#8fd8b5";

  return (
    <div style={{ display: "grid", gap: "10px" }} className={`markdown-body role-${role}`}>
      <ReactMarkdown 
        remarkPlugins={[remarkGfm]}
        components={{
          code({node, inline, className, children, ...props}: any) {
            const match = /language-(\w+)/.exec(className || '');
            return !inline ? (
              <div
                style={{
                  border: "1px solid rgba(121, 141, 168, 0.28)",
                  borderRadius: "14px",
                  overflow: "hidden",
                  background: "rgba(7, 11, 18, 0.92)",
                  marginTop: "8px",
                  marginBottom: "8px",
                }}
              >
                {match?.[1] ? (
                  <div
                    style={{
                      fontSize: "0.68rem",
                      letterSpacing: "0.05em",
                      textTransform: "uppercase",
                      color: accent,
                      padding: "8px 12px",
                      borderBottom: "1px solid rgba(121, 141, 168, 0.18)",
                      background: "rgba(17, 25, 38, 0.95)",
                    }}
                  >
                    {match[1]}
                  </div>
                ) : null}
                <pre
                  style={{
                    margin: 0,
                    padding: "12px 14px",
                    overflowX: "auto",
                    fontSize: "0.86rem",
                    lineHeight: 1.55,
                    color: "#edf3ff",
                    fontFamily:
                      "ui-monospace, SFMono-Regular, SF Mono, Menlo, Monaco, Consolas, Liberation Mono, monospace",
                  }}
                >
                  <code className={className} {...props}>
                    {children}
                  </code>
                </pre>
              </div>
            ) : (
              <code className={className} style={{
                background: "rgba(0,0,0,0.3)", padding: "2px 4px", borderRadius: "4px", fontFamily: "monospace"
              }} {...props}>
                {children}
              </code>
            );
          }
        }}
      >
        {text}
      </ReactMarkdown>
      {isStreaming ? (
        <div
          aria-label="Streaming response"
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: "6px",
            color: accent,
            fontSize: "0.8rem",
            fontFamily: "monospace",
          }}
        >
          <span
            style={{
              width: "7px",
              height: "7px",
              borderRadius: "999px",
              background: accent,
              display: "inline-block",
              opacity: 0.85,
            }}
          />
          streaming
        </div>
      ) : null}
    </div>
  );
}

interface ChatInterfaceProps {
  compactLayout: boolean;
  onSubmit: (task: string) => Promise<void>;
  onChoiceSelect: (messageId: string, option: string) => Promise<void>;
  onToggleMemory: () => void;
  onDeleteMemory: (id: string) => Promise<void>;
  onHardReset: () => Promise<void>;
  onOpenSettings: () => Promise<void>;
  memoryOverview: MemoryOverview | null;
  gatewaySessions: GatewaySessionRecord[];
  selectedSessionId: string | null;
  selectedSessionContext: GatewaySessionContext | null;
  onSelectSession: (sessionId: string) => Promise<void>;
  showAgents: boolean;
  onToggleAgents: () => void;
}

export default function ChatInterface({
  compactLayout,
  onSubmit,
  onChoiceSelect,
  onToggleMemory,
  onDeleteMemory,
  onHardReset,
  onOpenSettings,
  memoryOverview,
  gatewaySessions,
  selectedSessionId,
  selectedSessionContext,
  onSelectSession,
  showAgents,
  onToggleAgents,
}: ChatInterfaceProps) {
  const messages = useChatStore((state) => state.messages);
  const input = useChatStore((state) => state.input);
  const isBooting = useChatStore((state) => state.isBooting);
  const isProcessing = useChatStore((state) => state.isProcessing);
  const statusText = useChatStore((state) => state.statusText);
  const sessionTokens = useChatStore((state) => state.sessionTokens);
  const showMemory = useChatStore((state) => state.showMemory);
  const knowledgeEntries = useChatStore((state) => state.knowledgeEntries);
  const setInput = useChatStore((state) => state.setInput);
  const messagesScrollRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef(true);

  useEffect(() => {
    const pane = messagesScrollRef.current;
    if (!pane || !stickToBottomRef.current) {
      return;
    }
    const isStreamingActive = messages.some((m) => m.isStreaming);
    pane.scrollTo({
      top: pane.scrollHeight,
      behavior: messages.length <= 1 || isStreamingActive ? "auto" : "smooth",
    });
  }, [messages]);

  const updateScrollStickiness = () => {
    const pane = messagesScrollRef.current;
    if (!pane) {
      return;
    }
    const distanceFromBottom = pane.scrollHeight - pane.scrollTop - pane.clientHeight;
    stickToBottomRef.current = distanceFromBottom < 120;
  };

  const handleSubmit = async () => {
    const trimmed = input.trim();
    if (!trimmed || isBooting || isProcessing) return;
    await onSubmit(trimmed);
  };

  const handleCancel = async () => {
    try {
      await invoke("cancel_execution");
    } catch (_) {}
  };

  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        minWidth: 0,
        display: "flex",
        flexDirection: "column",
        background: "linear-gradient(180deg, #0f1014 0%, #0a0c10 100%)",
      }}
    >
      <header
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: compactLayout ? "flex-start" : "center",
          flexWrap: "wrap",
          gap: "18px",
          padding: "18px 22px",
          borderBottom: "1px solid #1c2432",
          background: "rgba(10, 12, 16, 0.9)",
          backdropFilter: "blur(12px)",
        }}
      >
        <div style={{ minWidth: 0, flex: "1 1 240px" }}>
          <div style={{ color: "#f4f7fb", fontSize: "1.35rem", fontWeight: 700 }}>
            MoatAgeex
          </div>
          <div style={{ color: "#8090ab", fontSize: "0.85rem" }}>
            Zero-trust swarm console over the Tauri IPC bridge
          </div>
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: compactLayout ? "flex-start" : "flex-end",
            flexWrap: "wrap",
            gap: "10px",
            flex: "1 1 320px",
          }}
        >
          <span
            style={{
              border: "1px solid #2c4d78",
              borderRadius: "999px",
              padding: "6px 12px",
              color: "#9dc0ff",
              fontFamily: "monospace",
              fontSize: "0.85rem",
              whiteSpace: "nowrap",
            }}
          >
            SESSION TOKENS: {sessionTokens}
          </span>
          <button
            type="button"
            onClick={() => void onOpenSettings()}
            disabled={isBooting || isProcessing}
            style={{
              border: "1px solid #2f3b4f",
              background: "#101722",
              color: isBooting || isProcessing ? "#596579" : "#edf3ff",
              borderRadius: "999px",
              padding: "9px 14px",
              cursor: isBooting || isProcessing ? "not-allowed" : "pointer",
            }}
          >
            Settings
          </button>
          <button
            type="button"
            onClick={onToggleMemory}
            disabled={isBooting}
            style={{
              border: "1px solid #2f3b4f",
              background: showMemory ? "#172132" : "#101722",
              color: isBooting ? "#596579" : "#edf3ff",
              borderRadius: "999px",
              padding: "9px 14px",
              cursor: isBooting ? "not-allowed" : "pointer",
            }}
          >
            {showMemory ? "Hide Memory" : "Memory"}
          </button>
          <button
            type="button"
            onClick={onToggleAgents}
            disabled={isBooting}
            style={{
              border: "1px solid #2f3b4f",
              background: showAgents ? "#172132" : "#101722",
              color: isBooting ? "#596579" : "#edf3ff",
              borderRadius: "999px",
              padding: "9px 14px",
              cursor: isBooting ? "not-allowed" : "pointer",
            }}
          >
            {showAgents ? "Hide Agents" : "Agents"}
          </button>
          <button
            type="button"
            onClick={() => void onHardReset()}
            disabled={isBooting || isProcessing}
            style={{
              border: "1px solid #7a2d2d",
              background: "#2e1212",
              color: isBooting || isProcessing ? "#8f7272" : "#ffc4c4",
              borderRadius: "999px",
              padding: "9px 14px",
              cursor: isBooting || isProcessing ? "not-allowed" : "pointer",
            }}
          >
            Reset Memory
          </button>
          <span
            style={{
              color: "#8b98af",
              fontSize: "0.82rem",
              fontWeight: 700,
            }}
          >
            ●
          </span>
        </div>
      </header>

      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: "flex",
          flexDirection: compactLayout ? "column" : "row",
        }}
      >
        <div
          style={{
            flex: 1,
            minWidth: 0,
            minHeight: 0,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div
            ref={messagesScrollRef}
            onScroll={updateScrollStickiness}
            style={{
              flex: 1,
              overflowY: "auto",
              padding: compactLayout ? "16px" : "22px",
              display: "flex",
              flexDirection: "column",
              gap: "14px",
              overscrollBehavior: "contain",
            }}
          >
            {messages.length === 0 ? (
              <div
                style={{
                  margin: "auto",
                  textAlign: "center",
                  color: "#76849c",
                  fontFamily: "monospace",
                }}
              >
                Object-capability hypervisor active.
              </div>
            ) : (
              messages.map((message) => (
                <div
                  key={message.id}
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    alignItems:
                      message.role === "user" ? "flex-end" : "flex-start",
                    gap: "6px",
                  }}
                >
                  <span
                    style={{
                      color: "#6f8099",
                      fontSize: "0.72rem",
                      textTransform: "uppercase",
                    }}
                  >
                    {message.role}
                  </span>
                  <div
                    style={{
                      width: "fit-content",
                      maxWidth: compactLayout ? "100%" : "78%",
                      padding: "14px 16px",
                      borderRadius: "18px",
                      background:
                        message.role === "user"
                          ? "#1e3a5a"
                          : message.role === "system"
                            ? "#3d1822"
                            : "#151c26",
                      color:
                        message.role === "system" ? "#ffc8d2" : "#f4f7fb",
                      border:
                        message.role === "user"
                          ? "1px solid #345f8a"
                          : message.role === "system"
                            ? "1px solid #703241"
                            : "1px solid #232f42",
                      whiteSpace: "pre-wrap",
                      wordBreak: "break-word",
                      boxShadow: "0 8px 24px rgba(0, 0, 0, 0.18)",
                    }}
                  >
                    <MessageBody
                      text={message.text}
                      isStreaming={message.isStreaming}
                      role={message.role}
                    />
                  </div>
                  {message.choiceOptions &&
                    message.choiceOptions.length > 0 &&
                    !message.choiceResolved && (
                      <div
                        style={{
                          display: "flex",
                          flexWrap: "wrap",
                          gap: "10px",
                          maxWidth: compactLayout ? "100%" : "78%",
                        }}
                      >
                        {message.choiceOptions.map((option) => (
                          <button
                            key={`${message.id}:${option}`}
                            type="button"
                            disabled={isProcessing || isBooting}
                            onClick={() => void onChoiceSelect(message.id, option)}
                            style={{
                              border: "1px solid #35507b",
                              background: "#102440",
                              color: "#dce8ff",
                              borderRadius: "999px",
                              padding: "10px 14px",
                              cursor:
                                isProcessing || isBooting ? "not-allowed" : "pointer",
                            }}
                          >
                            {option}
                          </button>
                        ))}
                      </div>
                    )}
                  {message.choiceResolved && message.selectedChoice && (
                    <div
                      style={{
                        color: "#86c09e",
                        fontSize: "0.8rem",
                        fontFamily: "monospace",
                      }}
                    >
                      Selected: {message.selectedChoice}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>

          {/* ── Status bar ── */}
          <div
            style={{
              borderTop: "1px solid #1c2432",
              background: "#0a101a",
              padding: "5px 16px",
              display: "flex",
              alignItems: "center",
              gap: "8px",
              minHeight: "28px",
              flexShrink: 0,
            }}
          >
            <span
              style={{
                width: "6px",
                height: "6px",
                borderRadius: "999px",
                background: isProcessing ? "#7ee0a1" : "#8b98af",
                flexShrink: 0,
                transition: "background 0.3s ease",
              }}
            />
            <span
              style={{
                color: "#6f8099",
                fontSize: "0.72rem",
                fontFamily: "monospace",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {isProcessing ? statusText : "○ Ready"}
            </span>
          </div>

          {/* ── Input bar ── */}
          <div
            style={{
              borderTop: "1px solid #1c2432",
              padding: compactLayout ? "16px" : "18px 22px",
              background: "rgba(12, 15, 20, 0.95)",
              position: "relative",
            }}
          >
            <div
              style={{
                display: "flex",
                gap: "12px",
                alignItems: "flex-end",
                flexDirection: compactLayout ? "column" : "row",
              }}
            >
              <textarea
                value={input}
                onChange={(event) => setInput(event.target.value)}
                onKeyDown={(event) => {
                  if (event.nativeEvent.isComposing) return;
                  if (event.key === "Enter" && !event.shiftKey) {
                    event.preventDefault();
                    void handleSubmit();
                  }
                }}
                disabled={isBooting || isProcessing}
                placeholder={
                  isBooting
                    ? "Booting MoatAgeex Kernel..."
                    : "Describe the next task in plain language"
                }
                style={{
                  flex: 1,
                  width: "100%",
                  minHeight: compactLayout ? "92px" : "76px",
                  resize: "vertical",
                  borderRadius: "16px",
                  border: "1px solid #263144",
                  background: "#0d121a",
                  color: "#f4f7fb",
                  padding: "14px 16px",
                  font: "inherit",
                  transition: "border-color 0.2s ease",
                }}
              />
              <div
                style={{
                  display: "flex",
                  gap: "8px",
                  width: compactLayout ? "100%" : undefined,
                }}
              >
                {/* ── Send / Stop button ── */}
                {isProcessing ? (
                  <button
                    type="button"
                    onClick={() => void handleCancel()}
                    style={{
                      border: "1px solid #7a2d2d",
                      background: "#3a1414",
                      color: "#ff9b9b",
                      borderRadius: "16px",
                      padding: "14px 18px",
                      flex: compactLayout ? 1 : undefined,
                      minWidth: "110px",
                      cursor: "pointer",
                      fontWeight: 700,
                    }}
                  >
                    ⏹ Stop
                  </button>
                ) : (
                  <button
                    type="button"
                    onClick={() => void handleSubmit()}
                    disabled={isBooting || !input.trim()}
                    style={{
                      border: "1px solid #2c588d",
                      background:
                        isBooting || !input.trim() ? "#152032" : "#173d68",
                      color:
                        isBooting || !input.trim() ? "#607086" : "#edf5ff",
                      borderRadius: "16px",
                      padding: "14px 18px",
                      flex: compactLayout ? 1 : undefined,
                      minWidth: "110px",
                      cursor:
                        isBooting || !input.trim() ? "not-allowed" : "pointer",
                      fontWeight: 700,
                    }}
                  >
                    {isBooting ? "Booting..." : "Send"}
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>

        {showMemory && (
          <aside
            style={{
              width: compactLayout ? "100%" : "320px",
              minWidth: compactLayout ? 0 : "320px",
              maxHeight: compactLayout ? "38vh" : undefined,
              borderLeft: compactLayout ? "none" : "1px solid #1c2432",
              borderTop: compactLayout ? "1px solid #1c2432" : "none",
              background: "#0d1117",
              display: "flex",
              flexDirection: "column",
              minHeight: 0,
            }}
          >
            <div
              style={{
                padding: "16px",
                borderBottom: "1px solid #1c2432",
                color: "#d9e8ff",
                fontWeight: 700,
              }}
            >
              Session Control Plane
            </div>
            <div
              style={{
                flex: 1,
                overflowY: "auto",
                padding: "12px",
                display: "flex",
                flexDirection: "column",
                gap: "10px",
                overscrollBehavior: "contain",
              }}
            >
              <div
                style={{
                  border: "1px solid #263144",
                  borderRadius: "14px",
                  background: "#101722",
                  padding: "12px",
                }}
              >
                <div
                  style={{
                    color: "#d9e8ff",
                    fontSize: "0.82rem",
                    fontWeight: 700,
                    marginBottom: "8px",
                  }}
                >
                  Live Sessions
                </div>
                {gatewaySessions.length === 0 ? (
                  <div style={{ color: "#7c8aa0", fontSize: "0.82rem" }}>
                    No gateway sessions recorded yet.
                  </div>
                ) : (
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "8px",
                      maxHeight: compactLayout ? "220px" : "260px",
                      overflowY: "auto",
                    }}
                  >
                    {gatewaySessions.map((session) => (
                      <button
                        key={session.sessionId}
                        type="button"
                        onClick={() => void onSelectSession(session.sessionId)}
                        style={{
                          border:
                            session.sessionId === selectedSessionId
                              ? "1px solid #4678c8"
                              : "1px solid #1f2b3b",
                          background:
                            session.sessionId === selectedSessionId ? "#16263d" : "#0d141d",
                          color: "#e6eefb",
                          borderRadius: "12px",
                          padding: "10px",
                          textAlign: "left",
                          cursor: "pointer",
                        }}
                      >
                        <div style={{ fontSize: "0.78rem", fontWeight: 700 }}>
                          {session.title}
                        </div>
                        <div style={{ color: "#8ea3c2", fontSize: "0.72rem", marginTop: "4px" }}>
                          {session.source}
                          {session.channel ? ` · ${session.channel}` : ""}
                        </div>
                        <div style={{ color: "#6f8099", fontSize: "0.72rem", marginTop: "4px" }}>
                          {session.totalMessages} msgs · {new Date(session.updatedAt * 1000).toLocaleString()}
                        </div>
                        {session.latestSummary && (
                          <div
                            style={{
                              color: "#b7c6db",
                              fontSize: "0.75rem",
                              marginTop: "6px",
                              whiteSpace: "pre-wrap",
                              wordBreak: "break-word",
                            }}
                          >
                            {session.latestSummary}
                          </div>
                        )}
                      </button>
                    ))}
                  </div>
                )}
              </div>

              <div
                style={{
                  border: "1px solid #263144",
                  borderRadius: "14px",
                  background: "#101722",
                  padding: "12px",
                }}
              >
                <div
                  style={{
                    color: "#d9e8ff",
                    fontSize: "0.82rem",
                    fontWeight: 700,
                    marginBottom: "8px",
                  }}
                >
                  Selected Session Context
                </div>
                {selectedSessionContext ? (
                  <div
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: "8px",
                    }}
                  >
                    {selectedSessionContext.latestSummary ? (
                      <div
                        style={{
                          border: "1px solid #1f2b3b",
                          borderRadius: "12px",
                          background: "#0d141d",
                          padding: "10px",
                        }}
                      >
                        <div
                          style={{
                            color: "#8ea3c2",
                            fontSize: "0.72rem",
                            textTransform: "uppercase",
                            marginBottom: "4px",
                          }}
                        >
                          Summary
                        </div>
                        <div
                          style={{
                            color: "#dce7f6",
                            fontSize: "0.8rem",
                            whiteSpace: "pre-wrap",
                            wordBreak: "break-word",
                          }}
                        >
                          {selectedSessionContext.latestSummary}
                        </div>
                      </div>
                    ) : null}
                    <div
                      style={{
                        border: "1px solid #1f2b3b",
                        borderRadius: "12px",
                        background: "#0d141d",
                        padding: "10px",
                      }}
                    >
                      <div
                        style={{
                          color: "#8ea3c2",
                          fontSize: "0.72rem",
                          textTransform: "uppercase",
                          marginBottom: "4px",
                        }}
                      >
                        Prompt-ready context
                      </div>
                      <div
                        style={{
                          color: "#dce7f6",
                          fontSize: "0.8rem",
                          whiteSpace: "pre-wrap",
                          wordBreak: "break-word",
                          maxHeight: compactLayout ? "220px" : "280px",
                          overflowY: "auto",
                        }}
                      >
                        {selectedSessionContext.context}
                      </div>
                    </div>
                  </div>
                ) : (
                  <div style={{ color: "#7c8aa0", fontSize: "0.82rem" }}>
                    Select a session to inspect its compacted context.
                  </div>
                )}
              </div>

              <div
                style={{
                  border: "1px solid #263144",
                  borderRadius: "14px",
                  background: "#101722",
                  padding: "12px",
                }}
              >
                <div
                  style={{
                    color: "#d9e8ff",
                    fontSize: "0.82rem",
                    fontWeight: 700,
                    marginBottom: "8px",
                  }}
                >
                  Durable Knowledge
                </div>
                {memoryOverview ? (
                  <div
                    style={{
                      color: "#8ea3c2",
                      fontSize: "0.76rem",
                      marginBottom: "10px",
                      lineHeight: 1.5,
                    }}
                  >
                    {memoryOverview.stats.knowledgeCount} knowledge entries ·{" "}
                    {memoryOverview.stats.sessionCount} sessions ·{" "}
                    {memoryOverview.stats.summarizedSessionCount} compacted
                    {memoryOverview.stats.lastCompactedAt
                      ? ` · last compaction ${new Date(
                          memoryOverview.stats.lastCompactedAt * 1000,
                        ).toLocaleString()}`
                      : ""}
                  </div>
                ) : null}
                {knowledgeEntries.length === 0 ? (
                <div style={{ color: "#7c8aa0", fontSize: "0.86rem" }}>
                  No durable knowledge records loaded.
                </div>
              ) : (
                knowledgeEntries.map((memory) => (
                  <div
                    key={memory.id}
                    style={{
                      border: "1px solid #263144",
                      borderRadius: "14px",
                      background: "#101722",
                      padding: "12px",
                      marginTop: "10px",
                    }}
                  >
                    <div
                      style={{
                        color: "#d9e8ff",
                        fontWeight: 600,
                        fontSize: "0.82rem",
                        marginBottom: "6px",
                        wordBreak: "break-word",
                      }}
                    >
                      {memory.title}
                    </div>
                    <div style={{ color: "#8ea3c2", fontSize: "0.72rem", marginBottom: "6px" }}>
                      {memory.kind} · {memory.source}
                    </div>
                    <div
                      style={{
                        color: "#aebbd1",
                        fontSize: "0.8rem",
                        whiteSpace: "pre-wrap",
                        wordBreak: "break-word",
                      }}
                    >
                      {memory.text}
                    </div>
                    <div
                      style={{
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        gap: "8px",
                        marginTop: "10px",
                      }}
                    >
                      <span style={{ color: "#76849c", fontSize: "0.74rem" }}>
                        {new Date(memory.timestamp * 1000).toLocaleString()}
                      </span>
                      <button
                        type="button"
                        onClick={() => void onDeleteMemory(memory.id)}
                        style={{
                          border: "1px solid #6c2c39",
                          background: "#2a1218",
                          color: "#ffc8d2",
                          borderRadius: "999px",
                          padding: "6px 10px",
                          cursor: "pointer",
                        }}
                      >
                        Delete
                      </button>
                    </div>
                  </div>
                ))
                )}
              </div>
            </div>
          </aside>
        )}
        {showAgents && (
          <ReasoningPanel />
        )}
      </div>

      {isBooting && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(3, 6, 10, 0.78)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 80,
            textAlign: "center",
            padding: "24px",
          }}
        >
          <div>
            <div style={{ color: "#f4f7fb", fontWeight: 700, marginBottom: "8px" }}>
              Booting MoatAgeex Kernel...
            </div>
            <div style={{ color: "#8da0bc", fontSize: "0.9rem" }}>
              The UI is ready. The Rust kernel is still initializing in the background.
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
