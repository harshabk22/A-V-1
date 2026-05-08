import { create } from "zustand";

export interface TelemetryEvent {
  id: string;
  kind: "status" | "ask_choice" | "stream";
  summary: string;
  raw: string;
  createdAt: number;
  agentId?: string;
  turn?: number;
  isPartial?: boolean;
}

interface TelemetryState {
  events: TelemetryEvent[];
  tick: number;
  pushEvent: (event: TelemetryEvent) => void;
  clear: () => void;
}

const MAX_EVENTS = 200;

export const useTelemetryStore = create<TelemetryState>((set) => ({
  events: [],
  tick: 0,
  pushEvent: (event) =>
    set((state) => ({
      events:
        state.events.length >= MAX_EVENTS
          ? [...state.events.slice(1), event]
          : [...state.events, event],
      tick: state.tick + 1,
    })),
  clear: () => set((state) => ({ events: [], tick: state.tick + 1 })),
}));
