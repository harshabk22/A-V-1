import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import ChatInterface from "./ChatInterface";
import ConsentModal from "./ConsentModal";
import SettingsModal from "./SettingsModal";
import { useChatStore } from "../stores/chatStore";
import { useReasoningStore } from "../stores/reasoningStore";
import { useConsentStore } from "../stores/consentStore";
import { useSettingsStore } from "../stores/settingsStore";
import {
  CONSENT_EVENT_NAME,
  LIVE_TELEMETRY_EVENT_NAME,
  GATEWAY_SERVER_EVENT_NAME,
  KERNEL_READY_EVENT_NAME,
  KERNEL_BOOTING_ERROR,
  REASONING_ENVELOPE_EVENT,
  REASONING_VIOLATION_EVENT,
  REASONING_YIELD_EVENT,
  REASONING_RESPONSE_EVENT,
  parseAskChoiceTelemetry,
  parseBackgroundErrorTelemetry,
  parseWorkerContextRequestTelemetry,
  type BackgroundErrorEvent,
  type ConsentRequest,
  type MemoryOverview,
  type GatewaySessionRecord,
  type GatewaySessionContext,
  type GatewayServerEvent,
  type AirgapEnvelopeEvent,
  type SecurityViolationEvent,
  type AgentYieldEvent,
  type AirgapResponseEvent,
  type SettingsState,
  type SettingsUpdate,
  type ExecuteTaskResponse,
} from "../types/ipc";

function makeId(prefix: string): string {
  return `${prefix}:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`;
}

function FaultBanner({
  fault,
  onDismiss,
}: {
  fault: BackgroundErrorEvent;
  onDismiss: () => void;
}) {
  const palette =
    fault.severity === "critical"
      ? { border: "#9d2f36", bg: "#3a1014", fg: "#ffd7db" }
      : fault.severity === "warning"
        ? { border: "#8a6a1b", bg: "#2c2210", fg: "#ffe8af" }
        : { border: "#6c3342", bg: "#2a1219", fg: "#ffd0da" };

  return (
    <div
      style={{
        border: `1px solid ${palette.border}`,
        background: palette.bg,
        color: palette.fg,
        borderRadius: "16px",
        padding: "12px 14px",
        display: "flex",
        justifyContent: "space-between",
        gap: "12px",
        alignItems: "flex-start",
      }}
    >
      <div style={{ minWidth: 0 }}>
        <div style={{ fontWeight: 700, marginBottom: "4px", textTransform: "uppercase" }}>
          {fault.severity} · {fault.subsystem}
        </div>
        <div style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{fault.message}</div>
        <div style={{ marginTop: "6px", fontFamily: "monospace", fontSize: "0.72rem", opacity: 0.82 }}>
          trace_id: {fault.trace_id}
        </div>
      </div>
      <button
        type="button"
        onClick={onDismiss}
        style={{
          border: `1px solid ${palette.border}`,
          background: "transparent",
          color: palette.fg,
          borderRadius: "999px",
          padding: "6px 10px",
          cursor: "pointer",
        }}
      >
        Dismiss
      </button>
    </div>
  );
}

export default function AppShell() {
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [isSavingSettings, setIsSavingSettings] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [faults, setFaults] = useState<BackgroundErrorEvent[]>([]);
  const [memoryOverview, setMemoryOverview] = useState<MemoryOverview | null>(null);
  const [gatewaySessions, setGatewaySessions] = useState<GatewaySessionRecord[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedSessionContext, setSelectedSessionContext] = useState<GatewaySessionContext | null>(
    null,
  );
  const selectedSessionIdRef = useRef<string | null>(null);
  const [isCompactLayout, setIsCompactLayout] = useState<boolean>(() =>
    typeof window !== "undefined" ? window.innerWidth < 1180 : false,
  );
  const [showAgents, setShowAgents] = useState(false);
  const setBooting = useChatStore((state) => state.setBooting);
  const setProcessing = useChatStore((state) => state.setProcessing);
  const setStatusText = useChatStore((state) => state.setStatusText);
  const setInput = useChatStore((state) => state.setInput);
  const addUserMessage = useChatStore((state) => state.addUserMessage);
  const addSystemMessage = useChatStore((state) => state.addSystemMessage);
  const addChoiceMessage = useChatStore((state) => state.addChoiceMessage);
  const resolveChoice = useChatStore((state) => state.resolveChoice);
  const upsertStreamingMessage = useChatStore((state) => state.upsertStreamingMessage);
  const setShowMemory = useChatStore((state) => state.setShowMemory);
  const setKnowledgeEntries = useChatStore((state) => state.setKnowledgeEntries);
  const addSessionTokens = useChatStore((state) => state.addSessionTokens);
  const clearConversation = useChatStore((state) => state.clearConversation);
  const settings = useSettingsStore((state) => state.settings);
  const setSettingsState = useSettingsStore((state) => state.setSettings);
  const applyRoutingUpdate = useSettingsStore((state) => state.applyRoutingUpdate);
  const clearSettingsState = useSettingsStore((state) => state.clearSettings);
  const addRequest = useConsentStore((state) => state.addRequest);

  useEffect(() => {
    selectedSessionIdRef.current = selectedSessionId;
  }, [selectedSessionId]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return undefined;
    }

    const media = window.matchMedia("(max-width: 1180px)");
    const handleChange = (event: MediaQueryListEvent | MediaQueryList) => {
      setIsCompactLayout(event.matches);
    };

    handleChange(media);
    if (typeof media.addEventListener === "function") {
      media.addEventListener("change", handleChange);
      return () => media.removeEventListener("change", handleChange);
    }

    media.addListener(handleChange);
    return () => media.removeListener(handleChange);
  }, []);

  useEffect(() => {
    let active = true;
    const unlisteners: UnlistenFn[] = [];

    const attach = async () => {
      unlisteners.push(
        await listen<ConsentRequest>(CONSENT_EVENT_NAME, (event) => {
          addRequest(event.payload);
          useChatStore.getState().setStatusText("⚠️ WAITING FOR USER CONSENT");
        }),
      );

      unlisteners.push(
        await listen<string>(LIVE_TELEMETRY_EVENT_NAME, (event) => {
          const payload = String(event.payload);

          const askChoice = parseAskChoiceTelemetry(payload);
          if (askChoice) {
            addChoiceMessage(askChoice.context, askChoice.options, askChoice.request_id);
            return;
          }

          const backgroundFault = parseBackgroundErrorTelemetry(payload);
          if (backgroundFault) {
            setFaults((current) => {
              const next = [backgroundFault, ...current];
              return next.slice(0, 5);
            });
            return;
          }

          if (payload.startsWith("__TOOL_STATUS__:")) {
            const statusMsg = payload.slice("__TOOL_STATUS__:".length);
            useChatStore.getState().setStatusText(statusMsg);
            return;
          }

          const workerContext = parseWorkerContextRequestTelemetry(payload);
          if (workerContext) {
            addSystemMessage(
              `[WORKER CONTEXT REQUEST]\n${workerContext.task_id} · ${workerContext.question}\nBlocking: ${workerContext.blocking ? "yes" : "no"} · Timeout: ${workerContext.timeout_secs}s`,
            );
            return;
          }
        }),
      );

      unlisteners.push(
        await listen(KERNEL_READY_EVENT_NAME, () => {
          setBooting(false);
          setStatusText("○ STANDBY");
        }),
      );

      unlisteners.push(
        await listen<GatewayServerEvent>(GATEWAY_SERVER_EVENT_NAME, (event) => {
          const gatewayEvent = event.payload;
          if (gatewayEvent.kind === "token_stream") {
            const streamKey = gatewayEvent.request_id ?? gatewayEvent.session_id ?? "stream:default";
            useChatStore.getState().upsertStreamingMessage({
              agent_id: streamKey,
              turn: 0,
              content: gatewayEvent.text,
              is_partial: true,
              role: "agent",
            });
            return;
          }

          if (
            gatewayEvent.kind !== "session_created" &&
            gatewayEvent.kind !== "session_message_appended" &&
            gatewayEvent.kind !== "session_compacted" &&
            gatewayEvent.kind !== "session_resolved"
          ) {
            return;
          }

          const preferredSessionId =
            selectedSessionIdRef.current ??
            ("session_id" in gatewayEvent ? gatewayEvent.session_id : null);

          void invoke<GatewaySessionRecord[]>("list_gateway_sessions", { limit: 24 })
            .then((sessions) => {
              if (!active) {
                return;
              }
              setGatewaySessions(sessions);
              const nextSessionId =
                preferredSessionId &&
                sessions.some((session) => session.sessionId === preferredSessionId)
                  ? preferredSessionId
                  : sessions[0]?.sessionId ?? null;
              setSelectedSessionId(nextSessionId);

              if (!nextSessionId) {
                setSelectedSessionContext(null);
                return;
              }

              void invoke<GatewaySessionContext>("get_gateway_session_context", {
                sessionId: nextSessionId,
                maxChars: 2200,
              })
                .then((context) => {
                  if (active) {
                    setSelectedSessionContext(context);
                  }
                })
                .catch(() => {
                  if (active) {
                    setSelectedSessionContext(null);
                  }
                });

              if (gatewayEvent.kind === "session_compacted") {
                void invoke<MemoryOverview>("get_memory_overview")
                  .then((overview) => {
                    if (active) {
                      setMemoryOverview(overview);
                      setKnowledgeEntries(overview.knowledge);
                    }
                  })
                  .catch(() => {
                    // keep background refresh failures quiet
                  });
              }
            })
            .catch(() => {
              // Keep gateway event failures quiet in the background; explicit user actions already surface hard errors.
            });
        }),
      );
      unlisteners.push(
        await listen<AirgapEnvelopeEvent>(REASONING_ENVELOPE_EVENT, (event) => {
          useReasoningStore.getState().handleEnvelopeEvent(event.payload);
        }),
      );
      unlisteners.push(
        await listen<SecurityViolationEvent>(REASONING_VIOLATION_EVENT, (event) => {
          useReasoningStore.getState().handleViolationEvent(event.payload);
        }),
      );
      unlisteners.push(
        await listen<AgentYieldEvent>(REASONING_YIELD_EVENT, (event) => {
          useReasoningStore.getState().handleYieldEvent(event.payload);
        }),
      );
      unlisteners.push(
        await listen<AirgapResponseEvent>(REASONING_RESPONSE_EVENT, (event) => {
          useReasoningStore.getState().handleAirgapResponseEvent(event.payload);
        }),
      );
    };

    void attach();

    void Promise.all([
      invoke<MemoryOverview>("get_memory_overview"),
      invoke<GatewaySessionRecord[]>("list_gateway_sessions", { limit: 16 }),
    ])
      .then(([overview, sessions]) => {
        if (!active) {
          return;
        }
        setMemoryOverview(overview);
        setKnowledgeEntries(overview.knowledge);
        setGatewaySessions(sessions);
        if (sessions.length > 0) {
          const initialSessionId = sessions[0].sessionId;
          setSelectedSessionId(initialSessionId);
          void invoke<GatewaySessionContext>("get_gateway_session_context", {
            sessionId: initialSessionId,
            maxChars: 2200,
          })
            .then((context) => {
              if (active) {
                setSelectedSessionContext(context);
              }
            })
            .catch(() => {
              if (active) {
                setSelectedSessionContext(null);
              }
            });
        }
        setBooting(false);
        setStatusText("○ STANDBY");
      })
      .catch((error) => {
        if (!active) {
          return;
        }
        if (!String(error).includes(KERNEL_BOOTING_ERROR)) {
          addSystemMessage(`[BOOT ERROR]\n${String(error)}`);
        }
      });

    return () => {
      active = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [
    addChoiceMessage,
    addSystemMessage,
    addRequest,
    setBooting,
    setKnowledgeEntries,
    setStatusText,
    upsertStreamingMessage,
  ]);

  const refreshMemoryOverview = async () => {
    try {
      const overview = await invoke<MemoryOverview>("get_memory_overview");
      setMemoryOverview(overview);
      setKnowledgeEntries(overview.knowledge);
    } catch (error) {
      addSystemMessage(`[MEMORY ERROR]\n${String(error)}`);
    }
  };

  const refreshGatewaySessions = async (preferredSessionId?: string | null) => {
    try {
      const sessions = await invoke<GatewaySessionRecord[]>("list_gateway_sessions", {
        limit: 24,
      });
      setGatewaySessions(sessions);
      const nextSessionId =
        preferredSessionId && sessions.some((session) => session.sessionId === preferredSessionId)
          ? preferredSessionId
          : sessions[0]?.sessionId ?? null;
      setSelectedSessionId(nextSessionId);
      if (nextSessionId) {
        const context = await invoke<GatewaySessionContext>("get_gateway_session_context", {
          sessionId: nextSessionId,
          maxChars: 2200,
        });
        setSelectedSessionContext(context);
      } else {
        setSelectedSessionContext(null);
      }
    } catch (error) {
      addSystemMessage(`[GATEWAY ERROR]\n${String(error)}`);
    }
  };

  const loadGatewaySession = async (sessionId: string) => {
    setSelectedSessionId(sessionId);
    try {
      const context = await invoke<GatewaySessionContext>("get_gateway_session_context", {
        sessionId,
        maxChars: 2200,
      });
      setSelectedSessionContext(context);
    } catch (error) {
      addSystemMessage(`[GATEWAY ERROR]\n${String(error)}`);
    }
  };

  const openSettings = async () => {
    const state = useChatStore.getState();
    if (state.isBooting) {
      return;
    }

    setSettingsError(null);
    setIsSettingsOpen(true);
    try {
      const [nextSettings] = await Promise.all([
        invoke<SettingsState>("get_settings"),
      ]);
      setSettingsState(nextSettings);
    } catch (error) {
      clearSettingsState();
      setSettingsError(String(error));
    }
  };

  const handleSaveSettings = async (update: SettingsUpdate) => {
    setIsSavingSettings(true);
    setSettingsError(null);
    try {
      await invoke("update_user_config", { update });
      applyRoutingUpdate(update);
      const nextSettings = await invoke<SettingsState>("get_settings");
      setSettingsState(nextSettings);
      setIsSettingsOpen(false);
      addSystemMessage("[SETTINGS UPDATED]\nRouting configuration saved to USER.md.");
    } catch (error) {
      setSettingsError(String(error));
    } finally {
      setIsSavingSettings(false);
    }
  };

  const submitTask = async (rawTask: string) => {
    const task = rawTask.trim();
    const state = useChatStore.getState();
    if (!task || state.isBooting) {
      return;
    }

    if (task.startsWith("nvapi-") && task.length > 30) {
      setInput("");
      setProcessing(true);
      setStatusText("● CONFIGURING NVIDIA ROUTER");
      try {
        await invoke("save_settings", {
          update: {
            nvidiaApiKey: task,
          }
        });
        
        // Also pre-fetch models so the LLM connection is fully established
        const models = await invoke<string[]>("fetch_nvidia_models", { apiKey: task });
        if (models.length > 0) {
          addSystemMessage(`[NVIDIA KEY ACCEPTED]\nConnected to the LLM successfully. Loaded ${models.length} routing models.\nYou can now chat.`);
        } else {
          addSystemMessage("[NVIDIA KEY ACCEPTED]\nKey saved, but failed to load model catalog.");
        }
      } catch (error) {
        addSystemMessage(`[CRITICAL ERROR]\nFailed to connect with LLM: ${error}`);
      } finally {
        setProcessing(false);
        setStatusText("○ STANDBY");
      }
      return;
    }

    setInput("");
    setProcessing(true);
    setStatusText("● RUST HYPERVISOR ACTIVE");
    addUserMessage(task);
    const requestStreamId = makeId("request");
    let resolvedSessionId = selectedSessionId;

    try {
      const kernelResponse = await invoke<ExecuteTaskResponse>("execute_task", {
        task,
        clientRequestId: requestStreamId,
      });
      if (kernelResponse.totalTokens > 0) {
        addSessionTokens(kernelResponse.totalTokens);
      }
      const cleanResponse = kernelResponse.text.trim();
      if (kernelResponse.sessionId) {
        resolvedSessionId = kernelResponse.sessionId;
        setSelectedSessionId(kernelResponse.sessionId);
      }
      if (!cleanResponse) {
        return;
      }

      useChatStore.getState().upsertStreamingMessage({
        agent_id: requestStreamId,
        turn: 0,
        content: cleanResponse,
        is_partial: false,
        role: kernelResponse.role,
      });
    } catch (error) {
      useChatStore.getState().upsertStreamingMessage({
        agent_id: requestStreamId,
        turn: 0,
        content: `[CRITICAL ERROR]\n${String(error)}`,
        is_partial: false,
        role: "system",
      });
    } finally {
      setProcessing(false);
      if (useConsentStore.getState().queue.length === 0) {
        setStatusText("○ STANDBY");
      }
      if (useChatStore.getState().showMemory) {
        await refreshMemoryOverview();
        await refreshGatewaySessions(resolvedSessionId);
      }
    }
  };

  const handleChoiceSelect = async (messageId: string, option: string) => {
    const state = useChatStore.getState();
    if (state.isBooting) {
      return;
    }
    // Find the message to get its backend request_id
    const message = state.messages.find((m) => m.id === messageId);
    resolveChoice(messageId, option);

    if (message?.choiceRequestId) {
      // True pause/resume: resolve the suspended backend oneshot
      try {
        await invoke("resolve_choice", {
          requestId: message.choiceRequestId,
          selection: option,
        });
      } catch (error) {
        addSystemMessage(`[CHOICE RESOLVE ERROR]\n${String(error)}`);
      }
    } else {
      // Legacy fallback: no request_id means old-style fire-and-forget
      setInput(option);
      await submitTask(option);
    }
  };

  const handleToggleMemory = async () => {
    const state = useChatStore.getState();
    if (state.isBooting) {
      return;
    }
    if (!state.showMemory) {
      await refreshMemoryOverview();
      await refreshGatewaySessions(selectedSessionId);
    }
    setShowMemory(!state.showMemory);
  };

  const handleToggleAgents = () => {
    const state = useChatStore.getState();
    if (state.isBooting) {
      return;
    }
    setShowAgents((prev) => !prev);
  };

  const handleDeleteKnowledgeMemory = async (id: string) => {
    try {
      await invoke("delete_knowledge_memory", { id });
      await refreshMemoryOverview();
    } catch (error) {
      addSystemMessage(`[MEMORY DELETE ERROR]\n${String(error)}`);
    }
  };

  const handleHardReset = async () => {
    const state = useChatStore.getState();
    if (state.isBooting || state.isProcessing) {
      return;
    }
    try {
      await invoke("nuke_database");
      clearConversation();
      useConsentStore.getState().clearQueue();
      setKnowledgeEntries([]);
      setMemoryOverview(null);
      setGatewaySessions([]);
      setSelectedSessionContext(null);
      setSelectedSessionId(null);
      setShowMemory(false);
      setFaults([]);
      setStatusText("○ STANDBY");
      addSystemMessage(
        "[MEMORY RESET]\nSession transcripts, summaries, and durable knowledge were cleared. Settings, provider keys, and permissions were preserved.",
      );
    } catch (error) {
      addSystemMessage(`[RESET ERROR]\n${String(error)}`);
    }
  };

  return (
    <div
      style={{
        width: "100vw",
        height: "100vh",
        display: "flex",
        flexDirection: isCompactLayout ? "column" : "row",
        background: "#09111d",
        color: "#f4f7fb",
        overflow: "hidden",
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
        {faults.length > 0 ? (
          <div
            style={{
              padding: "14px 16px 0",
              display: "grid",
              gap: "10px",
              background: "#09111d",
            }}
          >
            {faults.map((fault) => (
              <FaultBanner
                key={fault.trace_id}
                fault={fault}
                onDismiss={() =>
                  setFaults((current) =>
                    current.filter((entry) => entry.trace_id !== fault.trace_id),
                  )
                }
              />
            ))}
          </div>
        ) : null}
        <ChatInterface
        compactLayout={isCompactLayout}
        onSubmit={submitTask}
        onChoiceSelect={handleChoiceSelect}
        onToggleMemory={handleToggleMemory}
        onDeleteMemory={handleDeleteKnowledgeMemory}
        onHardReset={handleHardReset}
        onOpenSettings={openSettings}
        memoryOverview={memoryOverview}
        gatewaySessions={gatewaySessions}
        selectedSessionId={selectedSessionId}
        selectedSessionContext={selectedSessionContext}
        onSelectSession={loadGatewaySession}
        showAgents={showAgents}
        onToggleAgents={handleToggleAgents}
      />
      </div>
      <ConsentModal />
      <SettingsModal
        isOpen={isSettingsOpen}
        settings={settings}
        isSaving={isSavingSettings}
        isAutomationBusy={false}
        errorMessage={settingsError}
        onClose={() => {
          if (isSavingSettings) {
            return;
          }
          setIsSettingsOpen(false);
          setSettingsError(null);
        }}
        onSave={handleSaveSettings}
      />
    </div>
  );
}
