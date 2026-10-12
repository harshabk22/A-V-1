import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useReasoningStore, type AuditEntry } from "../stores/reasoningStore";

const AGENT_STATE_COLOR: Record<string, string> = {
  idle: "#8b98af",
  running: "#7ee0a1",
  yielded: "#ffbe5c",
  trapped: "#ff6b6b",
};

const AGENT_STATE_GLOW: Record<string, string> = {
  idle: "0 0 0px rgba(139,152,175,0)",
  running: "0 0 8px rgba(126,224,161,0.4)",
  yielded: "0 0 8px rgba(255,190,92,0.4)",
  trapped: "0 0 8px rgba(255,107,107,0.4)",
};

function AuditLine({ entry }: { entry: AuditEntry }) {
  const color =
    entry.kind === "violation"
      ? "#ff6b6b"
      : entry.kind === "yield"
        ? "#ffbe5c"
        : entry.kind === "envelope"
          ? "#8fd8b5"
          : entry.kind === "step"
            ? "#9dc0ff"
            : "#7c8aa0";

  return (
    <div
      style={{
        fontFamily: "monospace",
        fontSize: "0.72rem",
        color,
        padding: "4px 0",
        borderBottom: "1px solid rgba(28,36,50,0.6)",
        wordBreak: "break-all",
      }}
    >
      <span style={{ opacity: 0.5 }}>
        {new Date(entry.timestamp).toLocaleTimeString()}
      </span>{" "}
      {entry.message}
    </div>
  );
}

export default function ReasoningPanel() {
  const agents = useReasoningStore((s) => s.agents);
  const engineReady = useReasoningStore((s) => s.engineReady);
  const envelopeQueueSize = useReasoningStore((s) => s.envelopeQueueSize);
  const auditLog = useReasoningStore((s) => s.auditLog);
  const auditScrollRef = useRef<HTMLDivElement>(null);

  const [stepFuel, setStepFuel] = useState("1000000");
  const [uploadStatus, setUploadStatus] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const el = auditScrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [auditLog.length]);

  useEffect(() => {
    let active = true;
    const init = async () => {
      try {
        const ready = await invoke<boolean>("init_reasoning_engine");
        if (!active) return;
        useReasoningStore.getState().setEngineReady(ready);

        const agentIds = await invoke<string[]>("list_agents");
        if (!active) return;
        agentIds.forEach((id) => {
          useReasoningStore.getState().upsertAgent({
            agentId: id,
            state: "idle",
            fuelRemaining: 0,
            fuelBudget: 0,
            envelopesSent: 0,
            yieldMessage: "",
          });
        });
      } catch (e) {
        console.error("Failed to initialize reasoning engine:", e);
      }
    };
    void init();
    return () => {
      active = false;
    };
  }, []);

  const handleFileUpload = useCallback(async (file: File) => {
    setUploadStatus(`Reading ${file.name}...`);
    try {
      const wasmBase64 = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => {
          const res = (reader.result as string) || "";
          const commaIdx = res.indexOf(",");
          resolve(commaIdx >= 0 ? res.slice(commaIdx + 1) : res);
        };
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      const agentId = file.name.replace(/\.wasm$/i, "").replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 32) || "agent";

      setUploadStatus(`Instantiating ${agentId}...`);
      await invoke("instantiate_agent", { wasmBytesBase64: wasmBase64, agentLabel: agentId });
      setUploadStatus(`Agent ${agentId} instantiated.`);
      setTimeout(() => setUploadStatus(null), 3000);

      const agentIds = await invoke<string[]>("list_agents");
      agentIds.forEach((id) => {
        useReasoningStore.getState().upsertAgent({
          agentId: id,
          state: "idle",
          fuelRemaining: 0,
          fuelBudget: 0,
          envelopesSent: 0,
          yieldMessage: "",
        });
      });
    } catch (e) {
      setUploadStatus(`Error: ${String(e)}`);
      setTimeout(() => setUploadStatus(null), 5000);
    }
  }, []);

  const handleStep = useCallback(async (agentKey: string) => {
    try {
      useReasoningStore.getState().upsertAgent({
        agentId: agentKey,
        state: "running",
        fuelRemaining: 0,
        fuelBudget: 0,
        envelopesSent: 0,
        yieldMessage: "",
      });
      const fuel = parseInt(stepFuel, 10) || 1000000;
      const outcome = await invoke<{
        agentYielded: boolean;
        yieldMessage: string;
        totalRequestsSent: number;
        fuelUsed: number;
      }>("step_agent", { agentKey, fuelBudget: fuel });

      useReasoningStore.getState().upsertAgent({
        agentId: agentKey,
        state: outcome.agentYielded ? "yielded" : "idle",
        fuelRemaining: Math.max(0, fuel - outcome.fuelUsed),
        fuelBudget: fuel,
        envelopesSent: outcome.totalRequestsSent,
        yieldMessage: outcome.yieldMessage,
      });
    } catch (e) {
      useReasoningStore.getState().upsertAgent({
        agentId: agentKey,
        state: "trapped",
        fuelRemaining: 0,
        fuelBudget: 0,
        envelopesSent: 0,
        yieldMessage: String(e),
      });
    }
  }, [stepFuel]);

  const handleDestroy = useCallback(async (agentKey: string) => {
    try {
      await invoke("destroy_agent", { agentKey });
      useReasoningStore.getState().removeAgent(agentKey);
    } catch (e) {
      console.error("destroy_agent failed:", e);
    }
  }, []);

  return (
    <aside
      style={{
        width: "360px",
        minWidth: "360px",
        borderLeft: "1px solid #1c2432",
        background: "#0d1117",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      <div
        style={{
          padding: "16px",
          borderBottom: "1px solid #1c2432",
          color: "#d9e8ff",
          fontWeight: 700,
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
        }}
      >
        <span>Agent Sandbox</span>
        <span
          style={{
            fontSize: "0.68rem",
            color: engineReady ? "#7ee0a1" : "#8b98af",
            fontFamily: "monospace",
          }}
        >
          {engineReady ? "ENGINE READY" : "ENGINE OFFLINE"}
        </span>
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
        {/* Load WASM Section */}
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
            Load WASM
          </div>
          <input
            ref={fileInputRef}
            type="file"
            accept=".wasm"
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) handleFileUpload(file);
              e.target.value = "";
            }}
            style={{ display: "none" }}
          />
          <button
            onClick={() => fileInputRef.current?.click()}
            style={{
              width: "100%",
              padding: "8px 12px",
              borderRadius: "8px",
              border: "1px dashed #3d5278",
              background: "#0f1926",
              color: "#8fd8b5",
              cursor: "pointer",
              fontFamily: "monospace",
              fontSize: "0.78rem",
              fontWeight: 600,
            }}
          >
            + Upload .wasm
          </button>
          {uploadStatus && (
            <div
              style={{
                marginTop: "8px",
                color: uploadStatus.startsWith("Error") ? "#ff6b6b" : "#8fd8b5",
                fontSize: "0.72rem",
                fontFamily: "monospace",
              }}
            >
              {uploadStatus}
            </div>
          )}
        </div>

        {/* Fuel Budget */}
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
              marginBottom: "6px",
            }}
          >
            Step Fuel Budget
          </div>
          <input
            type="number"
            value={stepFuel}
            onChange={(e) => setStepFuel(e.target.value)}
            style={{
              width: "100%",
              padding: "6px 10px",
              borderRadius: "8px",
              border: "1px solid #263144",
              background: "#0d141d",
              color: "#d9e8ff",
              fontFamily: "monospace",
              fontSize: "0.8rem",
              boxSizing: "border-box",
            }}
            placeholder="1000000"
          />
        </div>

        {/* Agent Roster */}
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
              display: "flex",
              justifyContent: "space-between",
            }}
          >
            <span>Agent Roster</span>
            <span style={{ color: "#8ea3c2", fontSize: "0.76rem" }}>
              {agents.length} agent{agents.length !== 1 ? "s" : ""}
            </span>
          </div>
          {agents.length === 0 ? (
            <div style={{ color: "#7c8aa0", fontSize: "0.82rem" }}>
              No agents instantiated. Upload a .wasm file to begin.
            </div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
              {agents.map((agent) => (
                <div
                  key={agent.agentId}
                  style={{
                    border: "1px solid #1f2b3b",
                    borderRadius: "12px",
                    background: "#0d141d",
                    padding: "10px 12px",
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                      marginBottom: "6px",
                    }}
                  >
                    <span
                      style={{
                        color: "#e6eefb",
                        fontFamily: "monospace",
                        fontSize: "0.78rem",
                        fontWeight: 700,
                      }}
                    >
                      {agent.agentId.length > 20
                        ? `${agent.agentId.slice(0, 20)}...`
                        : agent.agentId}
                    </span>
                    <span
                      style={{
                        display: "inline-flex",
                        alignItems: "center",
                        gap: "5px",
                        fontSize: "0.7rem",
                        fontWeight: 700,
                        color: AGENT_STATE_COLOR[agent.state] ?? "#8b98af",
                        textTransform: "uppercase",
                      }}
                    >
                      <span
                        style={{
                          width: "7px",
                          height: "7px",
                          borderRadius: "999px",
                          background:
                            AGENT_STATE_COLOR[agent.state] ?? "#8b98af",
                          boxShadow:
                            AGENT_STATE_GLOW[agent.state] ??
                            "0 0 0px rgba(139,152,175,0)",
                        }}
                      />
                      {agent.state}
                    </span>
                  </div>

                  {/* Fuel gauge */}
                  {agent.fuelBudget > 0 && (
                    <div style={{ marginBottom: "4px" }}>
                      <div
                        style={{
                          color: "#8ea3c2",
                          fontSize: "0.68rem",
                          marginBottom: "3px",
                        }}
                      >
                        Fuel: {agent.fuelRemaining.toLocaleString()} /{" "}
                        {agent.fuelBudget.toLocaleString()}
                      </div>
                      <div
                        style={{
                          height: "4px",
                          borderRadius: "2px",
                          background: "#1f2b3b",
                          overflow: "hidden",
                        }}
                      >
                        <div
                          style={{
                            height: "100%",
                            width: `${Math.min(
                              100,
                              (agent.fuelRemaining / agent.fuelBudget) * 100,
                            )}%`,
                            background:
                              agent.fuelRemaining / agent.fuelBudget > 0.5
                                ? "linear-gradient(90deg, #2c8d5a, #4ad18b)"
                                : agent.fuelRemaining / agent.fuelBudget > 0.2
                                  ? "linear-gradient(90deg, #8d6a2c, #d1a84a)"
                                  : "linear-gradient(90deg, #8d2c2c, #d14a4a)",
                            borderRadius: "2px",
                            transition: "width 0.3s ease",
                          }}
                        />
                      </div>
                    </div>
                  )}

                  <div
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                      gap: "6px",
                    }}
                  >
                    <span
                      style={{
                        color: "#6f8099",
                        fontSize: "0.68rem",
                        fontFamily: "monospace",
                      }}
                    >
                      envelopes: {agent.envelopesSent}
                    </span>
                    <div style={{ display: "flex", gap: "4px" }}>
                      <button
                        onClick={() => handleStep(agent.agentId)}
                        style={{
                          padding: "3px 8px",
                          borderRadius: "6px",
                          border: "1px solid #2d5a3d",
                          background: "#152915",
                          color: "#7ee0a1",
                          cursor: "pointer",
                          fontSize: "0.66rem",
                          fontWeight: 700,
                          fontFamily: "monospace",
                        }}
                      >
                        STEP
                      </button>
                      <button
                        onClick={() => handleDestroy(agent.agentId)}
                        style={{
                          padding: "3px 8px",
                          borderRadius: "6px",
                          border: "1px solid #5a2d2d",
                          background: "#291515",
                          color: "#ff6b6b",
                          cursor: "pointer",
                          fontSize: "0.66rem",
                          fontWeight: 700,
                          fontFamily: "monospace",
                        }}
                      >
                        DESTROY
                      </button>
                    </div>
                  </div>

                  {agent.yieldMessage && (
                    <div
                      style={{
                        marginTop: "6px",
                        color: "#ffbe5c",
                        fontSize: "0.68rem",
                        fontFamily: "monospace",
                        wordBreak: "break-all",
                        maxHeight: "40px",
                        overflow: "hidden",
                      }}
                    >
                      {agent.yieldMessage}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Airgap Queue Summary */}
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
              marginBottom: "4px",
            }}
          >
            Airgap Queue
          </div>
          <div
            style={{
              color: envelopeQueueSize > 0 ? "#ffbe5c" : "#7c8aa0",
              fontFamily: "monospace",
              fontSize: "0.86rem",
            }}
          >
            {envelopeQueueSize} pending envelope
            {envelopeQueueSize !== 1 ? "s" : ""}
          </div>
        </div>

        {/* Audit Log */}
        <div
          style={{
            border: "1px solid #263144",
            borderRadius: "14px",
            background: "#101722",
            padding: "12px",
            flex: 1,
            minHeight: 0,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div
            style={{
              color: "#d9e8ff",
              fontSize: "0.82rem",
              fontWeight: 700,
              marginBottom: "8px",
              display: "flex",
              justifyContent: "space-between",
            }}
          >
            <span>Audit Log</span>
            <span style={{ color: "#6f8099", fontSize: "0.7rem" }}>
              {auditLog.length} entries
            </span>
          </div>
          <div
            ref={auditScrollRef}
            style={{
              flex: 1,
              overflowY: "auto",
              overscrollBehavior: "contain",
              minHeight: 0,
            }}
          >
            {auditLog.length === 0 ? (
              <div style={{ color: "#7c8aa0", fontSize: "0.78rem" }}>
                No events yet. Step an agent to see capability requests.
              </div>
            ) : (
              auditLog.map((entry) => (
                <AuditLine key={entry.id} entry={entry} />
              ))
            )}
          </div>
        </div>
      </div>
    </aside>
  );
}
