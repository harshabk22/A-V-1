import { useEffect, useRef } from "react";
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
          : "#9dc0ff";

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

  useEffect(() => {
    const el = auditScrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [auditLog.length]);

  return (
    <aside
      style={{
        width: "320px",
        minWidth: "320px",
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
                      {agent.agentId.length > 16
                        ? `${agent.agentId.slice(0, 16)}...`
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
                      gap: "16px",
                      color: "#6f8099",
                      fontSize: "0.68rem",
                      fontFamily: "monospace",
                    }}
                  >
                    <span>envelopes: {agent.envelopesSent}</span>
                    {agent.yieldMessage && (
                      <span
                        style={{
                          color: "#ffbe5c",
                          maxWidth: "120px",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        msg: {agent.yieldMessage}
                      </span>
                    )}
                  </div>
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