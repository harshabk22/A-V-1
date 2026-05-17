import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  SettingsState,
  SettingsUpdate,
} from "../types/ipc";

interface SettingsModalProps {
  isOpen: boolean;
  settings: SettingsState | null;
  isSaving: boolean;
  isAutomationBusy: boolean;
  errorMessage: string | null;
  onClose: () => void;
  onSave: (update: SettingsUpdate) => Promise<void>;
}

interface ProviderFieldState {
  value: string;
  remove: boolean;
}

type Provider = "nvidia" | "deepseek";

const PROVIDER_DEFAULTS: Record<Provider, { chat: string; planner: string; worker: string }> = {
  nvidia: {
    chat: "meta/llama-3.1-70b-instruct",
    planner: "meta/llama-3.1-405b-instruct",
    worker: "meta/llama-3.1-70b-instruct",
  },
  deepseek: {
    chat: "deepseek-chat",
    planner: "deepseek-chat",
    worker: "deepseek-chat",
  },
};

const PROVIDER_LABEL: Record<Provider, string> = {
  nvidia: "NVIDIA",
  deepseek: "DeepSeek",
};

function buildProviderState(): ProviderFieldState {
  return { value: "", remove: false };
}

export default function SettingsModal({
  isOpen,
  settings,
  isSaving,
  isAutomationBusy,
  errorMessage,
  onClose,
  onSave,
}: SettingsModalProps) {
  const [provider, setProvider] = useState<Provider>("nvidia");
  const [nvidia, setNvidia] = useState<ProviderFieldState>(buildProviderState);
  const [deepseek, setDeepseek] = useState<ProviderFieldState>(buildProviderState);
  const [chatModel, setChatModel] = useState("");
  const [plannerModel, setPlannerModel] = useState("");
  const [workerModel, setWorkerModel] = useState("");
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [isLoadingModels, setIsLoadingModels] = useState(false);
  const [isValidatingInference, setIsValidatingInference] = useState(false);
  const [modelCatalogError, setModelCatalogError] = useState("");

  const defaults = PROVIDER_DEFAULTS[provider];

  useEffect(() => {
    if (!isOpen || !settings) return;
    setProvider("nvidia");
    setNvidia(buildProviderState());
    setDeepseek(buildProviderState());
    setChatModel(settings.chatModel ?? defaults.chat);
    setPlannerModel(settings.plannerModel ?? defaults.planner);
    setWorkerModel(settings.workerModel ?? defaults.worker);
    setAvailableModels([]);
    setIsLoadingModels(false);
    setIsValidatingInference(false);
    setModelCatalogError("");
  }, [isOpen, settings]);

  const handleProviderSwitch = (next: Provider) => {
    setProvider(next);
    setAvailableModels([]);
    setModelCatalogError("");
    setChatModel(PROVIDER_DEFAULTS[next].chat);
    setPlannerModel(PROVIDER_DEFAULTS[next].planner);
    setWorkerModel(PROVIDER_DEFAULTS[next].worker);
  };

  if (!isOpen) return null;

  const isBusy = isSaving || isAutomationBusy || isValidatingInference;
  const currentField = provider === "nvidia" ? nvidia : deepseek;
  const setCurrentField = provider === "nvidia" ? setNvidia : setDeepseek;
  const hasStoredKey = provider === "nvidia" ? settings?.hasNvidia : settings?.hasDeepseek;

  const normalizeSelectedModel = (currentValue: string, catalog: string[], fallbackValue: string) => {
    const trimmed = currentValue.trim();
    if (trimmed && catalog.includes(trimmed)) return trimmed;
    if (catalog.includes(fallbackValue)) return fallbackValue;
    if (catalog[0]) return catalog[0];
    return trimmed || fallbackValue;
  };

  const handleLoadModels = async () => {
    const apiKey = currentField.value.trim() || (hasStoredKey ? "__stored__" : "");
    if (!apiKey) {
      setModelCatalogError(`Enter a ${PROVIDER_LABEL[provider]} API key or configure a stored one.`);
      setAvailableModels([]);
      
      return;
    }
    setIsLoadingModels(true);
    setModelCatalogError("");
    try {
      const cmd = provider === "nvidia" ? "fetch_nvidia_models" : "fetch_deepseek_models";
      const models = await invoke<string[]>(cmd, { apiKey });
      setAvailableModels(models);
      if (models.length === 0) {
        setModelCatalogError(`${PROVIDER_LABEL[provider]} returned an empty model catalog.`);
      } else {
        setChatModel((c) => normalizeSelectedModel(c, models, defaults.chat));
        setPlannerModel((c) => normalizeSelectedModel(c, models, defaults.planner));
        setWorkerModel((c) => normalizeSelectedModel(c, models, defaults.worker));
      }
    } catch (error) {
      setAvailableModels([]);
      
      setModelCatalogError(String(error));
    } finally {
      setIsLoadingModels(false);
    }
  };

  const handleSubmit = async () => {
    const normalizedChatModel = chatModel.trim() || defaults.chat;
    const normalizedPlannerModel = plannerModel.trim() || defaults.planner;
    const normalizedWorkerModel = workerModel.trim() || defaults.worker;
    const resolvedApiKey = currentField.value.trim();
    const shouldValidate = !currentField.remove || resolvedApiKey.length > 0;

    if (shouldValidate) {
      const validationApiKey = resolvedApiKey || (hasStoredKey ? "__stored__" : "");
      if (!validationApiKey) {
        setModelCatalogError(`Enter a ${PROVIDER_LABEL[provider]} API key before saving.`);
        return;
      }
      setIsValidatingInference(true);
      setModelCatalogError("");
      try {
        const uniqueModels = [...new Set([normalizedChatModel, normalizedPlannerModel, normalizedWorkerModel])];
        const validateCmd = provider === "nvidia" ? "validate_nvidia_inference" : "validate_deepseek_inference";
        for (const model of uniqueModels) {
          await invoke<boolean>(validateCmd, { apiKey: validationApiKey, modelName: model });
        }
      } catch (error) {
        setModelCatalogError(String(error));
        setIsValidatingInference(false);
        return;
      }
      setIsValidatingInference(false);
    }

    const nvidiaApiKey = provider === "nvidia" ? (nvidia.remove ? "" : resolvedApiKey || undefined) : undefined;
    const deepseekApiKey = provider === "deepseek" ? (deepseek.remove ? "" : resolvedApiKey || undefined) : undefined;

    await onSave({
      chatModel: normalizedChatModel,
      plannerModel: normalizedPlannerModel,
      workerModel: normalizedWorkerModel,
      nvidiaApiKey,
      deepseekApiKey,
      provider: provider,
    });
  };

  const renderModelSelect = (label: string, value: string, setValue: (next: string) => void, defaultValue: string) => (
    <div style={{ display: "grid", gap: "8px" }}>
      <label style={{ color: "#dfe8f6", fontSize: "0.88rem", fontWeight: 600 }}>{label}</label>
      <select
        value={value}
        onChange={(e) => setValue(e.target.value)}
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
          <option value={value || defaultValue}>{value || defaultValue || "Load models first..."}</option>
        ) : (
          availableModels.map((m) => (
            <option key={m} value={m}>{m}</option>
          ))
        )}
      </select>
      <div style={{ color: "#8ea0bb", fontSize: "0.78rem", lineHeight: 1.4 }}>
        {availableModels.length === 0
          ? "Enter an API key and load the live catalog to change this selection."
          : `Selected from the live ${PROVIDER_LABEL[provider]} catalog. Default: ${defaultValue}`}
      </div>
    </div>
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
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: "16px" }}>
          <div>
            <div style={{ color: "#f4f7fb", fontSize: "1.25rem", fontWeight: 700 }}>Settings</div>
            <div style={{ color: "#8ea0bb", fontSize: "0.88rem" }}>
              Provider routing configuration for your local USER.md
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

        {!settings ? (
          <div style={{ color: "#9eb0c7", fontFamily: "monospace" }}>Loading settings...</div>
        ) : (
          <>
            {/* Provider selector */}
            <div style={{ display: "flex", gap: "8px" }}>
              {(["nvidia", "deepseek"] as Provider[]).map((p) => (
                <button
                  key={p}
                  type="button"
                  onClick={() => handleProviderSwitch(p)}
                  disabled={isBusy}
                  style={{
                    flex: 1,
                    padding: "10px",
                    borderRadius: "12px",
                    border: provider === p ? "1px solid #3d6ba8" : "1px solid #263246",
                    background: provider === p ? "#13203a" : "#0d141d",
                    color: provider === p ? "#a5c4ff" : "#6f8099",
                    fontWeight: 700,
                    fontSize: "0.9rem",
                    cursor: isBusy ? "not-allowed" : "pointer",
                  }}
                >
                  {PROVIDER_LABEL[p]}
                </button>
              ))}
            </div>

            <section style={{ display: "grid", gap: "14px" }}>
              <div style={{ color: "#f4f7fb", fontSize: "1rem", fontWeight: 700 }}>
                {PROVIDER_LABEL[provider]} Routing
              </div>
              <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.6 }}>
                Settings are saved to your local <code style={{ color: "#a5c4ff" }}>USER.md</code>{" "}
                configuration.
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
                    {PROVIDER_LABEL[provider]} API Key
                  </label>
                  <div style={{ display: "flex", gap: "10px", alignItems: "center" }}>
                    <input
                      type="password"
                      value={currentField.value}
                      onChange={(e) => {
                        setCurrentField({ value: e.target.value, remove: false });
                        setAvailableModels([]);
                        setModelCatalogError("");
                        
                      }}
                      disabled={isBusy}
                      placeholder={hasStoredKey ? "•••••••• (Stored)" : `Enter ${PROVIDER_LABEL[provider]} API key`}
                      style={{
                        flex: 1,
                        borderRadius: "12px",
                        border: currentField.remove ? "1px solid #9f4747" : "1px solid #2d3a50",
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
                        cursor: isSaving || isLoadingModels || isValidatingInference ? "not-allowed" : "pointer",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {isLoadingModels ? "Loading..." : "Load Models"}
                    </button>
                    {hasStoredKey && (
                      <button
                        type="button"
                        onClick={() => {
                          setCurrentField({ value: "", remove: !currentField.remove });
                          setAvailableModels([]);
                          setModelCatalogError("");
                          
                        }}
                        disabled={isSaving || isValidatingInference}
                        style={{
                          border: "1px solid #7a2d2d",
                          background: currentField.remove ? "#432020" : "#2e1212",
                          color: "#ffc4c4",
                          borderRadius: "999px",
                          padding: "10px 12px",
                          cursor: isSaving || isValidatingInference ? "not-allowed" : "pointer",
                          whiteSpace: "nowrap",
                        }}
                      >
                        {currentField.remove ? "Undo Remove" : "Remove"}
                      </button>
                    )}
                  </div>
                  <div style={{ color: "#8ea0bb", fontSize: "0.8rem", lineHeight: 1.5 }}>
                    Leave blank to keep using a stored {PROVIDER_LABEL[provider]} key from USER.md,
                    the environment, or a saved local profile.
                  </div>
                </div>

                <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.5 }}>
                  {isValidatingInference
                    ? `Validating ${PROVIDER_LABEL[provider]} inference access...`
                    : isLoadingModels
                    ? `Loading the live ${PROVIDER_LABEL[provider]} model catalog...`
                    : availableModels.length > 0
                    ? `Loaded ${availableModels.length} ${PROVIDER_LABEL[provider]} models.`
                    : `Load the live ${PROVIDER_LABEL[provider]} catalog.`}
                </div>

                <div style={{ color: "#8ea0bb", fontSize: "0.84rem", lineHeight: 1.5 }}>
                  Recommended: chat <code style={{ color: "#a5c4ff" }}>{defaults.chat}</code>,
                  planner <code style={{ color: "#a5c4ff" }}>{defaults.planner}</code>,
                  worker <code style={{ color: "#a5c4ff" }}>{defaults.worker}</code>.
                </div>

                {renderModelSelect("Chat Model", chatModel, setChatModel, defaults.chat)}
                {renderModelSelect("Planner Model", plannerModel, setPlannerModel, defaults.planner)}
                {renderModelSelect("Worker Model", workerModel, setWorkerModel, defaults.worker)}
              </div>

              {currentField.remove && (
                <div style={{ color: "#ffb2b2", fontSize: "0.8rem" }}>
                  The stored {PROVIDER_LABEL[provider]} key will be removed from your local configuration on save.
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
            Cancel
          </button>
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
              cursor: isSaving || isValidatingInference || !settings ? "not-allowed" : "pointer",
            }}
          >
            {isSaving ? "Saving..." : isValidatingInference ? "Validating..." : "Save Settings"}
          </button>
        </div>
      </div>
    </div>
  );
}
