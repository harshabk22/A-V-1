import { create } from "zustand";
import type { SettingsState, SettingsUpdate } from "../types/ipc";

export const DEFAULT_CHAT_MODEL = "meta/llama-3.1-70b-instruct";
export const DEFAULT_PLANNER_MODEL = "meta/llama-3.1-405b-instruct";
export const DEFAULT_WORKER_MODEL = "meta/llama-3.1-70b-instruct";

interface SettingsStoreState {
  settings: SettingsState | null;
  chatModel: string;
  plannerModel: string;
  workerModel: string;
  setSettings: (settings: SettingsState | null) => void;
  applyRoutingUpdate: (update: SettingsUpdate) => void;
  clearSettings: () => void;
}

function deriveModels(settings: SettingsState | null) {
  return {
    chatModel: settings?.chatModel ?? DEFAULT_CHAT_MODEL,
    plannerModel: settings?.plannerModel ?? DEFAULT_PLANNER_MODEL,
    workerModel: settings?.workerModel ?? DEFAULT_WORKER_MODEL,
  };
}

export const useSettingsStore = create<SettingsStoreState>((set) => ({
  settings: null,
  ...deriveModels(null),
  setSettings: (settings) =>
    set({
      settings,
      ...deriveModels(settings),
    }),
  applyRoutingUpdate: (update) =>
    set((state) => ({
      settings: state.settings
        ? {
            ...state.settings,
            chatModel: update.chatModel ?? DEFAULT_CHAT_MODEL,
            plannerModel: update.plannerModel ?? DEFAULT_PLANNER_MODEL,
            workerModel: update.workerModel ?? DEFAULT_WORKER_MODEL,
          }
        : state.settings,
      chatModel: update.chatModel ?? DEFAULT_CHAT_MODEL,
      plannerModel: update.plannerModel ?? DEFAULT_PLANNER_MODEL,
      workerModel: update.workerModel ?? DEFAULT_WORKER_MODEL,
    })),
  clearSettings: () =>
    set({
      settings: null,
      ...deriveModels(null),
    }),
}));
