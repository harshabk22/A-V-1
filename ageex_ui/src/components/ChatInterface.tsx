import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useChatStore } from "../stores/chatStore";
import ReasoningPanel from "./ReasoningPanel";
import {
  type GatewaySessionContext,
  type MemoryOverview,
  type GatewaySessionRecord,
  KERNEL_READY_EVENT_NAME,
  type VoiceStatus,
} from "../types/ipc";

const VOICE_PREFIX_USER = "[USER VOICE]: ";
const VOICE_PREFIX_AGENT = "[AGENT VOICE]: ";
const TRANSCRIPT_CLEAR_MS = 4000;
const DEFAULT_VOICE_STATUS: VoiceStatus = {
  voiceEnabled: false,
  reason: "Checking voice subsystem...",
  modelPath: "",
};

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

/** Inline SVG microphone icon for the voice toggle button. */
function MicIcon({ active }: { active: boolean }) {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 24 24"
      fill="none"
      stroke={active ? "#ff5c5c" : "#b0c4de"}
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ display: "block" }}
    >
      <rect x="9" y="2" width="6" height="12" rx="3" />
      <path d="M5 10a7 7 0 0 0 14 0" />
      <line x1="12" y1="19" x2="12" y2="22" />
      <line x1="8" y1="22" x2="16" y2="22" />
    </svg>
  );
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
  const addSystemMessage = useChatStore((state) => state.addSystemMessage);
  const messagesScrollRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef(true);

  // ── Voice session state ──
  const [isVoiceActive, setIsVoiceActive] = useState(false);
  const [isVoicePending, setIsVoicePending] = useState(false);
  const [activeTranscript, setActiveTranscript] = useState("");
  const [transcriptRole, setTranscriptRole] = useState<"user" | "agent">("user");
  const [voiceStatus, setVoiceStatus] = useState<VoiceStatus>(DEFAULT_VOICE_STATUS);
  const transcriptTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let mounted = true;

    const refreshVoiceStatus = async () => {
      try {
        const status = await invoke<VoiceStatus>("check_voice_status");
        if (!mounted) {
          return;
        }
        setVoiceStatus(status);
        if (!status.voiceEnabled) {
          setIsVoiceActive(false);
        }
      } catch (err) {
        if (!mounted) {
          return;
        }
        setVoiceStatus({
          voiceEnabled: false,
          reason: `Voice status unavailable: ${String(err)}`,
          modelPath: "",
        });
        setIsVoiceActive(false);
      }
    };

    void refreshVoiceStatus();

    const unlistenPromise = listen(KERNEL_READY_EVENT_NAME, () => {
      void refreshVoiceStatus();
    });

    return () => {
      mounted = false;
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  // ── Telemetry listener for voice transcripts ──
  useEffect(() => {
    let mounted = true;
    const unlistenPromise = listen<string>("live-telemetry", (event) => {
      if (!mounted) return;
      const payload = String(event.payload);

      if (payload.startsWith("[VOICE ERROR]")) {
        const text = payload.slice("[VOICE ERROR]".length).trim();
        setIsVoiceActive(false);
        setActiveTranscript("");
        addSystemMessage(`[VOICE ERROR]\n${text}`);
        return;
      }

      if (payload.startsWith("[VOICE] Voice session stopped.")) {
        setIsVoiceActive(false);
        setActiveTranscript("");
        if (transcriptTimerRef.current) {
          clearTimeout(transcriptTimerRef.current);
          transcriptTimerRef.current = null;
        }
        return;
      }

      let text = "";
      let role: "user" | "agent" = "user";

      if (payload.startsWith(VOICE_PREFIX_USER)) {
        text = payload.slice(VOICE_PREFIX_USER.length).trim();
        role = "user";
      } else if (payload.startsWith(VOICE_PREFIX_AGENT)) {
        text = payload.slice(VOICE_PREFIX_AGENT.length).trim();
        role = "agent";
      } else {
        return; // Not a voice telemetry event
      }

      if (!text) return;

      setActiveTranscript(text);
      setTranscriptRole(role);

      // Auto-clear after TRANSCRIPT_CLEAR_MS
      if (transcriptTimerRef.current) {
        clearTimeout(transcriptTimerRef.current);
      }
      transcriptTimerRef.current = setTimeout(() => {
        if (mounted) {
          setActiveTranscript("");
        }
      }, TRANSCRIPT_CLEAR_MS);
    });

    return () => {
      mounted = false;
      if (transcriptTimerRef.current) {
        clearTimeout(transcriptTimerRef.current);
      }
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [addSystemMessage]);

  const handleToggleVoice = useCallback(async () => {
    if (isVoicePending) {
      return;
    }
    setIsVoicePending(true);
    try {
      if (!isVoiceActive) {
        await invoke("start_voice_session");
        setIsVoiceActive(true);
        addSystemMessage("[VOICE]\nVoice session started.");
      } else {
        await invoke("stop_voice_session");
        setIsVoiceActive(false);
        setActiveTranscript("");
        if (transcriptTimerRef.current) {
          clearTimeout(transcriptTimerRef.current);
          transcriptTimerRef.current = null;
        }
        addSystemMessage("[VOICE]\nVoice session stopped.");
      }
    } catch (err) {
      console.error("[Voice]", err);
      // If starting failed, make sure state is consistent
      setIsVoiceActive(false);
      addSystemMessage(`[VOICE ERROR]\n${String(err)}`);
    } finally {
      setIsVoicePending(false);
    }
  }, [addSystemMessage, isVoiceActive, isVoicePending]);

  useEffect(() => {
    const pane = messagesScrollRef.current;
    if (!pane || !stickToBottomRef.current) {
      return;
    }
    pane.scrollTo({
      top: pane.scrollHeight,
      behavior: messages.length <= 1 ? "auto" : "smooth",
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
    if (isBooting || isProcessing) {
      return;
    }
    await onSubmit(input);
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
      {/* ── Pulsing keyframe for hot mic (injected once) ── */}
      <style>{`
        @keyframes voicePulse {
          0%, 100% { box-shadow: 0 0 0 0 rgba(255, 72, 72, 0.55); }
          50% { box-shadow: 0 0 0 8px rgba(255, 72, 72, 0); }
        }
        @keyframes transcriptSlideIn {
          from { opacity: 0; transform: translateY(6px); }
          to { opacity: 1; transform: translateY(0); }
        }
      `}</style>

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
              color: statusText.includes("WAITING")
                ? "#ffbe5c"
                : statusText.includes("ACTIVE")
                ? "#7ee0a1"
                  : "#8b98af",
              fontSize: "0.82rem",
              fontWeight: 700,
              whiteSpace: compactLayout ? "normal" : "nowrap",
            }}
          >
            {statusText}
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

          {/* ── Input bar with voice controls ── */}
          <div
            style={{
              borderTop: "1px solid #1c2432",
              padding: compactLayout ? "16px" : "18px 22px",
              background: "rgba(12, 15, 20, 0.95)",
              position: "relative",
            }}
          >
            {/* ── Transcript overlay ── */}
            {isVoiceActive && activeTranscript && (
              <div
                style={{
                  position: "absolute",
                  bottom: "100%",
                  left: compactLayout ? "16px" : "22px",
                  right: compactLayout ? "16px" : "22px",
                  marginBottom: "8px",
                  padding: "10px 16px",
                  borderRadius: "14px",
                  background: "linear-gradient(135deg, rgba(20, 28, 44, 0.96), rgba(14, 20, 32, 0.96))",
                  border: transcriptRole === "user"
                    ? "1px solid rgba(90, 160, 255, 0.35)"
                    : "1px solid rgba(120, 230, 160, 0.35)",
                  backdropFilter: "blur(12px)",
                  boxShadow: "0 -4px 24px rgba(0, 0, 0, 0.3)",
                  animation: "transcriptSlideIn 0.2s ease-out",
                  zIndex: 10,
                }}
              >
                <div
                  style={{
                    fontSize: "0.68rem",
                    fontWeight: 700,
                    textTransform: "uppercase",
                    color: transcriptRole === "user" ? "#7ab8ff" : "#7ee0a1",
                    marginBottom: "4px",
                    letterSpacing: "0.04em",
                  }}
                >
                  {transcriptRole === "user" ? "🎙 You" : "🔊 Agent"}
                </div>
                <div
                  style={{
                    color: "#e8edf5",
                    fontSize: "0.92rem",
                    lineHeight: 1.4,
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-word",
                  }}
                >
                  {activeTranscript}
                </div>
              </div>
            )}

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
                  if (event.key === "Enter" && !event.shiftKey) {
                    event.preventDefault();
                    void handleSubmit();
                  }
                }}
                disabled={isBooting || isProcessing}
                placeholder={
                  isBooting
                    ? "Booting MoatAgeex Kernel..."
                    : isVoiceActive
                      ? "Voice session active — speak or type..."
                      : "Describe the next task in plain language"
                }
                style={{
                  flex: 1,
                  width: "100%",
                  minHeight: compactLayout ? "92px" : "76px",
                  resize: "vertical",
                  borderRadius: "16px",
                  border: isVoiceActive
                    ? "1px solid rgba(255, 92, 92, 0.4)"
                    : "1px solid #263144",
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
                {/* ── Microphone toggle button ── */}
                <button
                  id="voice-toggle-btn"
                  type="button"
                  onClick={() => void handleToggleVoice()}
                  disabled={isBooting || isVoicePending || !voiceStatus.voiceEnabled}
                  title={
                    isVoicePending
                      ? "Voice session is updating..."
                      : !voiceStatus.voiceEnabled
                        ? voiceStatus.reason ?? "Whisper model missing. Check terminal."
                      : isVoiceActive
                        ? "Stop voice session"
                        : "Start voice session"
                  }
                  style={{
                    border: isVoiceActive
                      ? "1px solid rgba(255, 72, 72, 0.6)"
                      : "1px solid #2f3b4f",
                    background: isVoiceActive
                      ? "rgba(80, 18, 18, 0.95)"
                      : "#101722",
                    borderRadius: "16px",
                    padding: "14px",
                    cursor:
                      isBooting || isVoicePending || !voiceStatus.voiceEnabled
                        ? "not-allowed"
                        : "pointer",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    minWidth: "48px",
                    transition: "all 0.2s ease",
                    animation: isVoiceActive
                      ? "voicePulse 1.8s ease-in-out infinite"
                      : "none",
                    opacity: !voiceStatus.voiceEnabled ? 0.55 : 1,
                  }}
                >
                  <MicIcon active={isVoiceActive} />
                </button>
                {/* ── Send button ── */}
                <button
                  type="button"
                  onClick={() => void handleSubmit()}
                  disabled={isBooting || isProcessing || !input.trim()}
                  style={{
                    border: "1px solid #2c588d",
                    background:
                      isBooting || isProcessing || !input.trim() ? "#152032" : "#173d68",
                    color:
                      isBooting || isProcessing || !input.trim() ? "#607086" : "#edf5ff",
                    borderRadius: "16px",
                    padding: "14px 18px",
                    flex: compactLayout ? 1 : undefined,
                    minWidth: "110px",
                    cursor:
                      isBooting || isProcessing || !input.trim() ? "not-allowed" : "pointer",
                    fontWeight: 700,
                  }}
                >
                  {isProcessing ? "Running..." : "Send"}
                </button>
              </div>
            </div>
            {!voiceStatus.voiceEnabled && (
              <div
                style={{
                  marginTop: "10px",
                  color: "#8b98af",
                  fontSize: "0.8rem",
                }}
              >
                Voice unavailable: {voiceStatus.reason ?? "Whisper model missing. Check terminal."}
              </div>
            )}
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
