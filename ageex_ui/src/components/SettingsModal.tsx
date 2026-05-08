import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  SentinelRuleRecord,
  SentinelTriggerType,
  SettingsState,
  SettingsUpdate,
  ShortcutRecord,
} from "../types/ipc";

interface SettingsModalProps {
  isOpen: boolean;
  settings: SettingsState | null;
  isSaving: boolean;
  isAutomationBusy: boolean;
  errorMessage: string | null;
  shortcuts: ShortcutRecord[];
  sentinelRules: SentinelRuleRecord[];
  onClose: () => void;
  onSave: (update: SettingsUpdate) => Promise<void>;
  onSaveShortcut: (triggerName: string, workflowJson: string) => Promise<void>;
  onDeleteShortcut: (triggerName: string) => Promise<void>;
  onAddSentinelRule: (
    triggerType: SentinelTriggerType,
    triggerConfig: string,
    targetShortcut: string,
  ) => Promise<void>;
  onToggleSentinelRule: (ruleId: string, isActive: boolean) => Promise<void>;
  onDeleteSentinelRule: (ruleId: string) => Promise<void>;
}

interface ProviderFieldState {
  value: string;
  remove: boolean;
}

const DEFAULT_CHAT_MODEL = "meta/llama-3.1-70b-instruct";
const DEFAULT_PLANNER_MODEL = "meta/llama-3.1-405b-instruct";
const DEFAULT_WORKER_MODEL = "meta/llama-3.1-70b-instruct";

function buildProviderState(): ProviderFieldState {
  return {
    value: "",
    remove: false,
  };
}

export default function SettingsModal({
  isOpen,
  settings,
  isSaving,
  isAutomationBusy,
  errorMessage,
  shortcuts,
  sentinelRules,
  onClose,
  onSave,
  onSaveShortcut,
  onDeleteShortcut,
  onAddSentinelRule,
  onToggleSentinelRule,
  onDeleteSentinelRule,
}: SettingsModalProps) {
  const [activeTab, setActiveTab] = useState<"core" | "automation">("core");
  const [nvidia, setNvidia] = useState<ProviderFieldState>(buildProviderState);
  const [shortcutTrigger, setShortcutTrigger] = useState("");
  const [shortcutWorkflowJson, setShortcutWorkflowJson] = useState("");
  const [sentinelTriggerType, setSentinelTriggerType] =
    useState<SentinelTriggerType>("CRON");
  const [sentinelTriggerConfig, setSentinelTriggerConfig] = useState("");
  const [sentinelTargetShortcut, setSentinelTargetShortcut] = useState("");
  const [chatModel, setChatModel] = useState("");
  const [plannerModel, setPlannerModel] = useState("");
  const [workerModel, setWorkerModel] = useState("");
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [isLoadingModels, setIsLoadingModels] = useState(false);
  const [isValidatingInference, setIsValidatingInference] = useState(false);
  const [modelCatalogError, setModelCatalogError] = useState("");
  const [catalogLoadedFrom, setCatalogLoadedFrom] = useState<"none" | "manual" | "stored">(
    "none",
  );

  useEffect(() => {
    if (!isOpen || !settings) {
      return;
    }
    setActiveTab("core");
    setNvidia(buildProviderState());
    setChatModel(settings.chatModel ?? DEFAULT_CHAT_MODEL);
    setPlannerModel(settings.plannerModel ?? DEFAULT_PLANNER_MODEL);
    setWorkerModel(settings.workerModel ?? DEFAULT_WORKER_MODEL);
    setAvailableModels([]);
    setIsLoadingModels(false);
    setIsValidatingInference(false);
    setModelCatalogError("");
    setCatalogLoadedFrom("none");
  }, [isOpen, settings]);

  if (!isOpen) {
    return null;
  }

  const isBusy = isSaving || isAutomationBusy || isValidatingInference;

  const normalizeSelectedModel = (
    currentValue: string,
    catalog: string[],
    fallbackValue: string,
  ) => {
    const trimmedCurrent = currentValue.trim();
    if (trimmedCurrent && catalog.includes(trimmedCurrent)) {
      return trimmedCurrent;
    }
    if (catalog.includes(fallbackValue)) {
      return fallbackValue;
    }
    if (catalog[0]) {
      return catalog[0];
    }
    return trimmedCurrent || fallbackValue;
  };

  const handleLoadModels = async () => {
    const apiKey = nvidia.value.trim() || (settings?.hasNvidia ? "__stored__" : "");
    if (!apiKey) {
      setModelCatalogError("Enter a NVIDIA API key or configure a stored one before loading models.");
      setAvailableModels([]);
      setCatalogLoadedFrom("none");
      return;
    }

    setIsLoadingModels(true);
    setModelCatalogError("");
    try {
      const models = await invoke<string[]>("fetch_nvidia_models", { apiKey });
      setAvailableModels(models);
      setCatalogLoadedFrom(nvidia.value.trim() ? "manual" : "stored");
      if (models.length === 0) {
        setModelCatalogError("NVIDIA returned an empty model catalog for this key.");
      } else {
        setChatModel((current) =>
          normalizeSelectedModel(current, models, DEFAULT_CHAT_MODEL),
        );
        setPlannerModel((current) =>
          normalizeSelectedModel(current, models, DEFAULT_PLANNER_MODEL),
        );
        setWorkerModel((current) =>
          normalizeSelectedModel(current, models, DEFAULT_WORKER_MODEL),
        );
      }
    } catch (error) {
      setAvailableModels([]);
      setCatalogLoadedFrom("none");
      setModelCatalogError(String(error));
    } finally {
      setIsLoadingModels(false);
    }
  };

  const handleSubmit = async () => {
    const normalizedChatModel = chatModel.trim() || DEFAULT_CHAT_MODEL;
    const normalizedPlannerModel = plannerModel.trim() || DEFAULT_PLANNER_MODEL;
    const normalizedWorkerModel = workerModel.trim() || DEFAULT_WORKER_MODEL;
    const resolvedApiKey = nvidia.value.trim();
    const shouldValidateInference = !nvidia.remove || resolvedApiKey.length > 0;

    if (shouldValidateInference) {
      const validationApiKey =
        resolvedApiKey || (settings?.hasNvidia ? "__stored__" : "");
      if (!validationApiKey) {
        setModelCatalogError(
          "Enter a NVIDIA API key before saving, or keep an existing stored key available for inference.",
        );
        return;
      }

      setIsValidatingInference(true);
      setModelCatalogError("");
      try {
        const uniqueModels = [...new Set([
          normalizedChatModel,
          normalizedPlannerModel,
          normalizedWorkerModel,
        ])];
        for (const model of uniqueModels) {
          await invoke<boolean>("validate_nvidia_inference", {
            apiKey: validationApiKey,
            modelName: model,
          });
        }
      } catch (error) {
        setModelCatalogError(String(error));
        setIsValidatingInference(false);
        return;
      }
      setIsValidatingInference(false);
    }

    await onSave({
      chatModel: normalizedChatModel,
      plannerModel: normalizedPlannerModel,
      workerModel: normalizedWorkerModel,
      nvidiaApiKey: nvidia.remove ? "" : resolvedApiKey || undefined,
    });
  };

  const handleSaveShortcut = async () => {
    await onSaveShortcut(shortcutTrigger, shortcutWorkflowJson);
    setShortcutTrigger("");
    setShortcutWorkflowJson("");
  };

  const handleAddSentinelRule = async () => {
    await onAddSentinelRule(
      sentinelTriggerType,
      sentinelTriggerConfig,
      sentinelTargetShortcut,
    );
    setSentinelTriggerConfig("");
    setSentinelTargetShortcut("");
  };

  const renderModelSelect = (
    label: string,
    value: string,
    setValue: (next: string) => void,
    defaultValue: string,
  ) => (
    <div style={{ display: "grid", gap: "8px" }}>
      <label style={{ color: "#dfe8f6", fontSize: "0.88rem", fontWeight: 600 }}>
        {label}
      </label>
      <select
        value={value}
        onChange={(event) => setValue(event.target.value)}
        disabled={isSaving || isLoadingModels || availableModels.length === 0}
        style={{
          borderRadius: "12px",
          border: "1px solid #2d3a50",
          background: "#0c1420",
          color: "#f4f7fb",
          padding: "11px 14px",
        }}
      >
        {availableModels.length === 0 ? (
          <option value={value || defaultValue}>
            {value || defaultValue || "Enter API Key to load models..."}
          </option>
        ) : (
          availableModels.map((model) => (
            <option key={model} value={model}>
              {model}
            </option>
          ))
        )}
      </select>
      <div style={{ color: "#8ea0bb", fontSize: "0.78rem", lineHeight: 1.4 }}>
        {availableModels.length === 0
          ? "Enter an API key and load the live NVIDIA catalog to change this selection."
          : `Selected from the live NVIDIA model catalog. Default: ${defaultValue}`}
      </div>
    </div>
  );

  const renderAutomationSection = () => (
    <section style={{ display: "grid", gap: "18px" }}>
      <section style={{ display: "grid", gap: "12px" }}>
        <div style={{ color: "#f4f7fb", fontSize: "1rem", fontWeight: 700 }}>
          Shortcuts
        </div>
        <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.5 }}>
          Save pre-compiled workflow JSON behind a slash trigger. Running the shortcut routes the
          workflow through the session assistant and the normal worker, MetaBrain, and consent
          pipeline.
        </div>
        <div
          style={{
            display: "grid",
            gap: "12px",
            border: "1px solid #283547",
            borderRadius: "16px",
            background: "#101722",
            padding: "14px",
          }}
        >
          <input
            type="text"
            value={shortcutTrigger}
            onChange={(event) => setShortcutTrigger(event.target.value)}
            placeholder="/daily-report"
            disabled={isBusy}
            style={{
              borderRadius: "12px",
              border: "1px solid #2d3a50",
              background: "#0c1420",
              color: "#f4f7fb",
              padding: "11px 14px",
            }}
          />
          <textarea
            value={shortcutWorkflowJson}
            onChange={(event) => setShortcutWorkflowJson(event.target.value)}
            placeholder='{\"workflow_id\":\"...\",\"session_id\":\"...\",\"global_context\":\"...\",\"global_context_token_estimate\":10,\"tasks\":[]}'
            disabled={isBusy}
            rows={8}
            style={{
              borderRadius: "12px",
              border: "1px solid #2d3a50",
              background: "#0c1420",
              color: "#f4f7fb",
              padding: "11px 14px",
              resize: "vertical",
              fontFamily: "monospace",
              fontSize: "0.84rem",
            }}
          />
          <div style={{ display: "flex", justifyContent: "flex-end" }}>
            <button
              type="button"
              onClick={() => void handleSaveShortcut()}
              disabled={isBusy}
              style={{
                border: "1px solid #2b6f4e",
                background: "#123522",
                color: "#c8ffdb",
                borderRadius: "999px",
                padding: "10px 16px",
                cursor: isBusy ? "not-allowed" : "pointer",
              }}
            >
              Save Shortcut
            </button>
          </div>
        </div>
        <div style={{ display: "grid", gap: "10px" }}>
          {shortcuts.length === 0 ? (
            <div style={{ color: "#9eb0c7", fontSize: "0.85rem" }}>
              No shortcuts saved yet.
            </div>
          ) : (
            shortcuts.map((shortcut) => (
              <div
                key={shortcut.triggerName}
                style={{
                  display: "grid",
                  gap: "10px",
                  border: "1px solid #283547",
                  borderRadius: "16px",
                  padding: "14px",
                  background: "#101722",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                    gap: "12px",
                    flexWrap: "wrap",
                  }}
                >
                  <div>
                    <div style={{ color: "#dfe8f6", fontWeight: 700 }}>
                      {shortcut.triggerName}
                    </div>
                    <div style={{ color: "#8ea0bb", fontSize: "0.78rem" }}>
                      Created {shortcut.createdAt}
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => void onDeleteShortcut(shortcut.triggerName)}
                    disabled={isBusy}
                    style={{
                      border: "1px solid #7a2d2d",
                      background: "#2e1212",
                      color: "#ffc4c4",
                      borderRadius: "999px",
                      padding: "8px 12px",
                      cursor: isBusy ? "not-allowed" : "pointer",
                    }}
                  >
                    Delete
                  </button>
                </div>
                <pre
                  style={{
                    margin: 0,
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-word",
                    color: "#b9c6da",
                    fontSize: "0.76rem",
                    fontFamily: "monospace",
                  }}
                >
                  {shortcut.workflowJson}
                </pre>
              </div>
            ))
          )}
        </div>
      </section>

      <section style={{ display: "grid", gap: "12px" }}>
        <div style={{ color: "#f4f7fb", fontSize: "1rem", fontWeight: 700 }}>
          Sentinel Rules
        </div>
        <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.5 }}>
          Background rules only synthesize shortcut triggers. The resulting workflow still pauses
          on consent if the relevant MetaBrain policy does not exist.
        </div>
        <div
          style={{
            display: "grid",
            gap: "12px",
            border: "1px solid #283547",
            borderRadius: "16px",
            background: "#101722",
            padding: "14px",
          }}
        >
          <select
            value={sentinelTriggerType}
            onChange={(event) =>
              setSentinelTriggerType(event.target.value as SentinelTriggerType)
            }
            disabled={isBusy}
            style={{
              borderRadius: "12px",
              border: "1px solid #2d3a50",
              background: "#0c1420",
              color: "#f4f7fb",
              padding: "11px 14px",
            }}
          >
            <option value="CRON">CRON</option>
            <option value="FILE_WATCH">FILE_WATCH</option>
          </select>
          <input
            type="text"
            value={sentinelTriggerConfig}
            onChange={(event) => setSentinelTriggerConfig(event.target.value)}
            placeholder={
              sentinelTriggerType === "CRON"
                ? "0 17 * * 5"
                : "/Users/name/Downloads/*.pdf"
            }
            disabled={isBusy}
            style={{
              borderRadius: "12px",
              border: "1px solid #2d3a50",
              background: "#0c1420",
              color: "#f4f7fb",
              padding: "11px 14px",
            }}
          />
          <input
            type="text"
            value={sentinelTargetShortcut}
            onChange={(event) => setSentinelTargetShortcut(event.target.value)}
            placeholder="/process-pdf"
            disabled={isBusy}
            style={{
              borderRadius: "12px",
              border: "1px solid #2d3a50",
              background: "#0c1420",
              color: "#f4f7fb",
              padding: "11px 14px",
            }}
          />
          <div style={{ display: "flex", justifyContent: "flex-end" }}>
            <button
              type="button"
              onClick={() => void handleAddSentinelRule()}
              disabled={isBusy}
              style={{
                border: "1px solid #2b6f4e",
                background: "#123522",
                color: "#c8ffdb",
                borderRadius: "999px",
                padding: "10px 16px",
                cursor: isBusy ? "not-allowed" : "pointer",
              }}
            >
              Add Sentinel Rule
            </button>
          </div>
        </div>
        <div style={{ display: "grid", gap: "10px" }}>
          {sentinelRules.length === 0 ? (
            <div style={{ color: "#9eb0c7", fontSize: "0.85rem" }}>
              No sentinel rules configured yet.
            </div>
          ) : (
            sentinelRules.map((rule) => (
              <div
                key={rule.ruleId}
                style={{
                  display: "grid",
                  gap: "10px",
                  border: "1px solid #283547",
                  borderRadius: "16px",
                  padding: "14px",
                  background: "#101722",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                    gap: "12px",
                    flexWrap: "wrap",
                  }}
                >
                  <div style={{ display: "grid", gap: "4px" }}>
                    <div style={{ color: "#dfe8f6", fontWeight: 700 }}>
                      {rule.triggerType} → {rule.targetShortcut}
                    </div>
                    <div style={{ color: "#8ea0bb", fontSize: "0.78rem" }}>
                      {rule.triggerConfig}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: "8px", flexWrap: "wrap" }}>
                    <button
                      type="button"
                      onClick={() => void onToggleSentinelRule(rule.ruleId, !rule.isActive)}
                      disabled={isBusy}
                      style={{
                        border: "1px solid #314158",
                        background: rule.isActive ? "#162944" : "#121b28",
                        color: rule.isActive ? "#dce8ff" : "#dce6f5",
                        borderRadius: "999px",
                        padding: "8px 12px",
                        cursor: isBusy ? "not-allowed" : "pointer",
                      }}
                    >
                      {rule.isActive ? "Disable" : "Enable"}
                    </button>
                    <button
                      type="button"
                      onClick={() => void onDeleteSentinelRule(rule.ruleId)}
                      disabled={isBusy}
                      style={{
                        border: "1px solid #7a2d2d",
                        background: "#2e1212",
                        color: "#ffc4c4",
                        borderRadius: "999px",
                        padding: "8px 12px",
                        cursor: isBusy ? "not-allowed" : "pointer",
                      }}
                    >
                      Delete
                    </button>
                  </div>
                </div>
                <div style={{ color: rule.isActive ? "#92f2af" : "#8ea0bb", fontSize: "0.8rem" }}>
                  {rule.isActive ? "Active" : "Inactive"}
                </div>
              </div>
            ))
          )}
        </div>
      </section>
    </section>
  );

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(4, 9, 16, 0.82)",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        padding: "24px",
        zIndex: 110,
        overflowY: "auto",
      }}
    >
      <div
        style={{
          width: "min(760px, 100%)",
          maxWidth: "760px",
          maxHeight: "calc(100vh - 48px)",
          overflowY: "auto",
          background: "#0f131a",
          border: "1px solid #263246",
          borderRadius: "20px",
          boxShadow: "0 22px 56px rgba(0, 0, 0, 0.45)",
          padding: "24px",
          display: "grid",
          gap: "22px",
          margin: "auto",
          overscrollBehavior: "contain",
        }}
      >
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: "16px",
          }}
        >
          <div>
            <div style={{ color: "#f4f7fb", fontSize: "1.25rem", fontWeight: 700 }}>
              Settings
            </div>
            <div style={{ color: "#8ea0bb", fontSize: "0.88rem" }}>
              NVIDIA-first routing configuration for your local USER.md
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            disabled={isSaving}
            style={{
              border: "1px solid #314158",
              background: "#121b28",
              color: "#dce6f5",
              borderRadius: "999px",
              padding: "9px 14px",
              cursor: isSaving ? "not-allowed" : "pointer",
            }}
          >
            Close
          </button>
        </div>

        <div style={{ display: "flex", gap: "10px", flexWrap: "wrap" }}>
          {[
            { id: "core" as const, label: "Core Settings" },
            { id: "automation" as const, label: "Automation" },
          ].map((tab) => {
            const selected = activeTab === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                onClick={() => setActiveTab(tab.id)}
                disabled={isBusy}
                style={{
                  border: selected ? "1px solid #4b86e8" : "1px solid #2d3a50",
                  background: selected ? "#162944" : "#0c1420",
                  color: selected ? "#dce8ff" : "#b5c3d9",
                  borderRadius: "999px",
                  padding: "10px 14px",
                  cursor: isBusy ? "not-allowed" : "pointer",
                  fontWeight: 600,
                }}
              >
                {tab.label}
              </button>
            );
          })}
        </div>

        {!settings ? (
          <div style={{ color: "#9eb0c7", fontFamily: "monospace" }}>
            Loading settings...
          </div>
        ) : activeTab === "core" ? (
          <>
            <section style={{ display: "grid", gap: "14px" }}>
              <div style={{ color: "#f4f7fb", fontSize: "1rem", fontWeight: 700 }}>
                NVIDIA Routing Configuration
              </div>
              <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.6 }}>
                Settings are saved to your local <code style={{ color: "#a5c4ff" }}>USER.md</code>{" "}
                configuration. This form controls the NVIDIA routing models used by chat, planner,
                and workers.
              </div>
              <div
                style={{
                  display: "grid",
                  gap: "16px",
                  border: "1px solid #283547",
                  borderRadius: "18px",
                  background: "rgba(16, 23, 34, 0.88)",
                  backdropFilter: "blur(14px)",
                  padding: "18px",
                  boxShadow: "inset 0 1px 0 rgba(255, 255, 255, 0.03)",
                }}
              >
                <div style={{ display: "grid", gap: "8px" }}>
                  <label style={{ color: "#dfe8f6", fontSize: "0.88rem", fontWeight: 600 }}>
                    NVIDIA API Key
                  </label>
                  <div style={{ display: "flex", gap: "10px", alignItems: "center" }}>
                    <input
                      type="password"
                      value={nvidia.value}
                      onChange={(event) => {
                        setNvidia({
                          value: event.target.value,
                          remove: false,
                        });
                        setAvailableModels([]);
                        setModelCatalogError("");
                        setCatalogLoadedFrom("none");
                      }}
                      disabled={isBusy}
                      placeholder={settings.hasNvidia ? "•••••••• (Stored)" : "Enter NVIDIA API key"}
                      style={{
                        flex: 1,
                        borderRadius: "12px",
                        border: nvidia.remove ? "1px solid #9f4747" : "1px solid #2d3a50",
                        background: "#0c1420",
                        color: "#f4f7fb",
                        padding: "11px 14px",
                      }}
                    />
                    <button
                      type="button"
                      onClick={() => void handleLoadModels()}
                      disabled={isSaving || isLoadingModels || isValidatingInference}
                      style={{
                        border: "1px solid #314158",
                        background: "#121b28",
                        color: "#dce6f5",
                        borderRadius: "999px",
                        padding: "10px 12px",
                        cursor:
                          isSaving || isLoadingModels || isValidatingInference
                            ? "not-allowed"
                            : "pointer",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {isLoadingModels ? "Loading..." : "Load Models"}
                    </button>
                    {settings.hasNvidia && (
                      <button
                        type="button"
                        onClick={() => {
                          setNvidia({
                            value: "",
                            remove: !nvidia.remove,
                          });
                          setAvailableModels([]);
                          setModelCatalogError("");
                          setCatalogLoadedFrom("none");
                        }}
                      disabled={isSaving || isValidatingInference}
                      style={{
                        border: "1px solid #7a2d2d",
                        background: nvidia.remove ? "#432020" : "#2e1212",
                        color: "#ffc4c4",
                        borderRadius: "999px",
                        padding: "10px 12px",
                        cursor:
                          isSaving || isValidatingInference ? "not-allowed" : "pointer",
                        whiteSpace: "nowrap",
                      }}
                    >
                        {nvidia.remove ? "Undo Remove" : "Remove"}
                      </button>
                    )}
                  </div>
                  <div style={{ color: "#8ea0bb", fontSize: "0.8rem", lineHeight: 1.5 }}>
                    Leave this blank to keep using a stored NVIDIA key from USER.md, the
                    environment, or a saved local profile.
                  </div>
                </div>

                <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.5 }}>
                  {isValidatingInference
                    ? "Validating NVIDIA inference access for the selected routing models..."
                    : isLoadingModels
                    ? "Loading the live NVIDIA model catalog..."
                    : availableModels.length > 0
                      ? `Loaded ${availableModels.length} NVIDIA models using the ${catalogLoadedFrom === "manual" ? "typed" : "stored"} API key.`
                      : "Load the live NVIDIA catalog to populate the routing model dropdowns."}
                </div>

                <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.5 }}>
                  Recommended defaults: chat{" "}
                  <code style={{ color: "#a5c4ff" }}>{DEFAULT_CHAT_MODEL}</code>, planner{" "}
                  <code style={{ color: "#a5c4ff" }}>{DEFAULT_PLANNER_MODEL}</code>, worker{" "}
                  <code style={{ color: "#a5c4ff" }}>{DEFAULT_WORKER_MODEL}</code>.
                </div>

                {renderModelSelect("Chat Model", chatModel, setChatModel, DEFAULT_CHAT_MODEL)}
                {renderModelSelect(
                  "Planner Model",
                  plannerModel,
                  setPlannerModel,
                  DEFAULT_PLANNER_MODEL,
                )}
                {renderModelSelect(
                  "Worker Model",
                  workerModel,
                  setWorkerModel,
                  DEFAULT_WORKER_MODEL,
                )}
              </div>

              {nvidia.remove && (
                <div style={{ color: "#ffb2b2", fontSize: "0.8rem" }}>
                  The stored NVIDIA key reference will be removed from your local configuration on
                  save.
                </div>
              )}

              {modelCatalogError && (
                <div
                  style={{
                    borderRadius: "12px",
                    border: "1px solid #7a2d2d",
                    background: "#2a1010",
                    color: "#ffc4c4",
                    padding: "10px 14px",
                    fontSize: "0.84rem",
                    fontWeight: 500,
                  }}
                >
                  {modelCatalogError}
                </div>
              )}
            </section>
          </>
        ) : (
          renderAutomationSection()
        )}

        {errorMessage && (
          <div
            style={{
              borderRadius: "14px",
              border: "1px solid #7a2d2d",
              background: "#2a1010",
              color: "#ffc4c4",
              padding: "12px 14px",
              whiteSpace: "pre-wrap",
            }}
          >
            {errorMessage}
          </div>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: "12px" }}>
          <button
            type="button"
            onClick={onClose}
            disabled={isBusy}
            style={{
              border: "1px solid #314158",
              background: "#121b28",
              color: "#dce6f5",
              borderRadius: "999px",
              padding: "10px 16px",
              cursor: isBusy ? "not-allowed" : "pointer",
            }}
          >
            {activeTab === "automation" ? "Done" : "Cancel"}
          </button>
          {activeTab === "core" && (
            <button
              type="button"
              onClick={() => void handleSubmit()}
              disabled={isSaving || isValidatingInference || !settings}
              style={{
                border: "1px solid #2b6f4e",
                background: "#123522",
                color: "#c8ffdb",
                borderRadius: "999px",
                padding: "10px 16px",
                cursor:
                  isSaving || isValidatingInference || !settings
                    ? "not-allowed"
                    : "pointer",
              }}
            >
              {isSaving
                ? "Saving..."
                : isValidatingInference
                  ? "Validating..."
                  : "Save Settings"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
