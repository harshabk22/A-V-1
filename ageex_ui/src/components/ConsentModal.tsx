import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useConsentStore } from "../stores/consentStore";
import { useChatStore } from "../stores/chatStore";
import type { ConsentDecision } from "../types/ipc";

type IntegrityState = "checking" | "valid" | "tampered" | "identical_hashes";

function base64ToBytes(base64: string): Uint8Array {
  const binary = window.atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

async function sha256Hex(input: string): Promise<string> {
  const bytes = new TextEncoder().encode(input);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

async function sha256HexFromBase64(base64: string): Promise<string> {
  const bytes = base64ToBytes(base64);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

export default function ConsentModal() {
  const queue = useConsentStore((state) => state.queue);
  const isResolving = useConsentStore((state) => state.isResolving);
  const setResolving = useConsentStore((state) => state.setResolving);
  const removeRequest = useConsentStore((state) => state.removeRequest);
  const setStatusText = useChatStore((state) => state.setStatusText);
  const addSystemMessage = useChatStore((state) => state.addSystemMessage);
  const [integrityState, setIntegrityState] = useState<IntegrityState>("checking");

  const pending = queue.length > 0 ? queue[0] : null;
  const backlogCount = queue.length - 1;

  useEffect(() => {
    if (!pending) {
      setIntegrityState("checking");
      return;
    }

    let cancelled = false;
    const verify = async () => {
      if (pending.summary_hash === pending.payload_hash) {
        if (!cancelled) {
          setIntegrityState("identical_hashes");
        }
        return;
      }

      try {
        const [computedSummaryHash, computedPayloadHash] = await Promise.all([
          sha256Hex(pending.summary),
          sha256HexFromBase64(pending.payload),
        ]);

        if (cancelled) {
          return;
        }

        if (
          computedSummaryHash === pending.summary_hash &&
          computedPayloadHash === pending.payload_hash
        ) {
          setIntegrityState("valid");
        } else {
          setIntegrityState("tampered");
        }
      } catch {
        if (!cancelled) {
          setIntegrityState("tampered");
        }
      }
    };

    setIntegrityState("checking");
    void verify();

    return () => {
      cancelled = true;
    };
  }, [pending]);

  useEffect(() => {
    if (!pending) {
      return;
    }

    const blockEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
      }
    };

    window.addEventListener("keydown", blockEscape, true);
    return () => window.removeEventListener("keydown", blockEscape, true);
  }, [pending]);

  const payloadPreview = useMemo(() => {
    if (!pending) {
      return "";
    }
    try {
      return new TextDecoder().decode(base64ToBytes(pending.payload));
    } catch {
      return "[Unable to decode execution payload]";
    }
  }, [pending]);

  if (!pending) {
    return null;
  }

  const approvalAllowed = integrityState === "valid" && !isResolving;

  const handleDecision = async (decision: ConsentDecision) => {
    if (isResolving) {
      return;
    }

    setResolving(true);
    let resolved = false;
    try {
      await invoke("resolve_consent", {
        requestId: pending.request_id,
        decision,
      });
      resolved = true;
      if (queue.length <= 1) {
        setStatusText("● RUST HYPERVISOR ACTIVE");
      }
    } catch (error) {
      addSystemMessage(`[CONSENT ERROR]\n${String(error)}`);
    } finally {
      if (resolved) {
        removeRequest(pending.request_id);
      }
      setResolving(false);
    }
  };

  const integrityMessage =
    integrityState === "checking"
      ? "Verifying cryptographic consent payload..."
      : integrityState === "valid"
        ? "Integrity verified. Summary and raw payload hashes are valid and distinct."
        : integrityState === "identical_hashes"
          ? "Approval disabled: summary and payload hashes are identical. This request is invalid."
          : "Approval disabled: consent payload failed local integrity verification.";

  const integrityColor =
    integrityState === "valid"
      ? "#9ef4bc"
      : integrityState === "checking"
        ? "#f7c76a"
        : "#ff9a9a";

  const isSubSpawnPolicy = Boolean(pending.sub_spawn_policy);
  const isSkillReview = Boolean(pending.skill_review);

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(2, 8, 18, 0.82)",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        zIndex: 100,
        padding: "24px",
        overflowY: "auto",
      }}
      aria-modal="true"
      role="dialog"
    >
      <div
        style={{
          width: "min(560px, 100%)",
          maxHeight: "calc(100vh - 48px)",
          overflowY: "auto",
          borderRadius: "20px",
          border: "1px solid #5e4b1f",
          background: "#121212",
          boxShadow: "0 18px 48px rgba(0, 0, 0, 0.45)",
          padding: "24px",
          margin: "auto",
        }}
      >
        <div style={{ color: "#f7c76a", fontWeight: 700, marginBottom: "12px", display: "flex", alignItems: "center", gap: "10px" }}>
          Capability Execution Requested
          {backlogCount > 0 && (
            <span
              style={{
                fontSize: "0.72rem",
                fontWeight: 600,
                padding: "3px 10px",
                borderRadius: "999px",
                background: "rgba(247, 199, 106, 0.15)",
                border: "1px solid rgba(247, 199, 106, 0.3)",
                color: "#f7c76a",
              }}
            >
              Request 1 of {queue.length}
            </span>
          )}
        </div>
        <div style={{ color: "#ececec", marginBottom: "8px" }}>
          <strong>{pending.action}</strong> needs approval.
        </div>
        <div style={{ color: "#76849c", fontSize: "0.82rem", marginBottom: "12px" }}>
          Session: {pending.session_id}
        </div>
        <div
          style={{
            marginBottom: "12px",
            padding: "12px",
            borderRadius: "12px",
            background: "#171717",
            border: "1px solid #272727",
            color: "#f4f4f4",
          }}
        >
          {pending.summary}
        </div>
        {pending.rule_review ? (
          <div
            style={{
              display: "grid",
              gap: "12px",
              marginBottom: "12px",
            }}
          >
            <div
              style={{
                border: "1px solid #2a3240",
                borderRadius: "14px",
                background: "#0f141c",
                padding: "12px",
              }}
            >
              <div style={{ fontWeight: 700, color: "#f4f7fb", marginBottom: "6px" }}>
                Proposed Rule
              </div>
              <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
                Worker: {pending.rule_review.new_rule.target_worker}
              </div>
              <div style={{ color: "#f4f7fb", marginTop: "8px", whiteSpace: "pre-wrap" }}>
                {pending.rule_review.new_rule.rule_text}
              </div>
            </div>
            <div
              style={{
                border: "1px solid #2a3240",
                borderRadius: "14px",
                background: "#0f141c",
                padding: "12px",
              }}
            >
              <div style={{ fontWeight: 700, color: "#f4f7fb", marginBottom: "6px" }}>
                Conflicting Rules
              </div>
              {pending.rule_review.conflicting_rules.length === 0 ? (
                <div style={{ color: "#9fb0c6", fontSize: "0.88rem" }}>
                  No direct conflicts detected.
                </div>
              ) : (
                <div style={{ display: "grid", gap: "10px" }}>
                  {pending.rule_review.conflicting_rules.map((rule) => (
                    <div
                      key={rule.id}
                      style={{
                        border: "1px solid #283444",
                        borderRadius: "12px",
                        padding: "10px",
                        background: "#0a1018",
                      }}
                    >
                      <div style={{ color: "#c6d2e3", fontSize: "0.82rem", marginBottom: "4px" }}>
                        Worker: {rule.target_worker}
                      </div>
                      <div style={{ color: "#f4f7fb", whiteSpace: "pre-wrap" }}>
                        {rule.rule_text}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        ) : null}
        {pending.sub_spawn_policy ? (
          <div
            style={{
              display: "grid",
              gap: "10px",
              marginBottom: "12px",
              padding: "12px",
              borderRadius: "14px",
              border: "1px solid #2a3240",
              background: "#0f141c",
            }}
          >
            <div style={{ color: "#f4f7fb", fontWeight: 700 }}>
              Sub-Agent Policy Request
            </div>
            <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
              Source Worker: {pending.sub_spawn_policy.source_worker}
            </div>
            <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
              Target Worker: {pending.sub_spawn_policy.target_worker}
            </div>
            <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
              Tool: {pending.sub_spawn_policy.allowed_tool}
            </div>
          </div>
        ) : null}
        {pending.skill_review ? (
          <div
            style={{
              display: "grid",
              gap: "10px",
              marginBottom: "12px",
              padding: "12px",
              borderRadius: "14px",
              border: "1px solid #2a3240",
              background: "#0f141c",
            }}
          >
            <div style={{ color: "#f4f7fb", fontWeight: 700 }}>
              Skill Installation Review
            </div>
            <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
              Skill: {pending.skill_review.skill_name}
            </div>
            <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
              Language: {pending.skill_review.language}
            </div>
            <div style={{ color: "#c6d2e3", fontSize: "0.88rem" }}>
              Code Hash: {pending.skill_review.code_hash}
            </div>
            <div style={{ color: "#f4f7fb", whiteSpace: "pre-wrap" }}>
              {pending.skill_review.description}
            </div>
            <div
              style={{
                border: "1px solid #283444",
                borderRadius: "12px",
                padding: "10px",
                background: "#0a1018",
              }}
            >
              <div style={{ color: "#c6d2e3", fontSize: "0.82rem", marginBottom: "6px" }}>
                Parameter Schema
              </div>
              <pre
                style={{
                  margin: 0,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  color: "#dce9ff",
                  fontFamily: "monospace",
                  fontSize: "0.86rem",
                }}
              >
                {pending.skill_review.parameter_schema}
              </pre>
            </div>
            <div
              style={{
                border: "1px solid #283444",
                borderRadius: "12px",
                padding: "10px",
                background: "#0a1018",
              }}
            >
              <div style={{ color: "#c6d2e3", fontSize: "0.82rem", marginBottom: "6px" }}>
                Source Code
              </div>
              <pre
                style={{
                  margin: 0,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  color: "#9ef4bc",
                  fontFamily: "monospace",
                  fontSize: "0.86rem",
                  maxHeight: "30vh",
                  overflowY: "auto",
                }}
              >
                <code>{pending.skill_review.source_code}</code>
              </pre>
            </div>
          </div>
        ) : null}
        <pre
          style={{
            margin: 0,
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            background: "#090909",
            border: "1px solid #272727",
            borderRadius: "14px",
            color: "#9ef4bc",
            padding: "14px",
            fontFamily: "monospace",
            fontSize: "0.88rem",
            maxHeight: "28vh",
            overflowY: "auto",
          }}
        >
          {payloadPreview}
        </pre>
        <div
          style={{
            display: "grid",
            gap: "8px",
            marginTop: "12px",
            fontFamily: "monospace",
            fontSize: "0.78rem",
            color: integrityColor,
          }}
        >
          <div>summary_hash: {pending.summary_hash}</div>
          <div>payload_hash: {pending.payload_hash}</div>
          <div>{integrityMessage}</div>
        </div>
        <div
          style={{
            display: "flex",
            gap: "12px",
            justifyContent: "flex-end",
            marginTop: "20px",
          }}
        >
          <button
            type="button"
            onClick={() => void handleDecision("deny")}
            disabled={isResolving}
            style={{
              border: "1px solid #7b2f2f",
              background: "#3c1111",
              color: "#ffc5c5",
              borderRadius: "999px",
              padding: "10px 16px",
              cursor: isResolving ? "wait" : "pointer",
            }}
          >
            Deny
          </button>
          {isSubSpawnPolicy && pending.sub_spawn_policy?.can_create_permanent_policy ? (
            <button
              type="button"
              onClick={() => void handleDecision("create_permanent_policy")}
              disabled={!approvalAllowed}
              style={{
                border: "1px solid #39628f",
                background: "#12253f",
                color: approvalAllowed ? "#c7e0ff" : "#6f7d8c",
                borderRadius: "999px",
                padding: "10px 16px",
                cursor: approvalAllowed ? "pointer" : "not-allowed",
              }}
            >
              Always Allow
            </button>
          ) : null}
          <button
            type="button"
            onClick={() => void handleDecision("approve_once")}
            disabled={!approvalAllowed}
            style={{
              border: "1px solid #2f6d41",
              background: "#10331b",
              color: approvalAllowed ? "#beffd2" : "#6f8c77",
              borderRadius: "999px",
              padding: "10px 16px",
              cursor: approvalAllowed ? "pointer" : "not-allowed",
            }}
            >
            {isSkillReview
              ? "Approve & Install Skill"
              : isSubSpawnPolicy
                ? "Approve Once"
                : "Approve"}
          </button>
        </div>
      </div>
    </div>
  );
}
