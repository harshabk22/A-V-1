import { create } from "zustand";
import type {
  AgentStatus,
  AirgapEnvelopeEvent,
  SecurityViolationEvent,
  AgentYieldEvent,
  AirgapResponseEvent,
} from "../types/ipc";

export interface AuditEntry {
  id: string;
  timestamp: number;
  kind: "envelope" | "violation" | "yield" | "step";
  agentId: string;
  message: string;
}

interface ReasoningState {
  agents: AgentStatus[];
  engineReady: boolean;
  envelopeQueueSize: number;
  auditLog: AuditEntry[];
  isStepPending: boolean;
  setEngineReady: (ready: boolean) => void;
  upsertAgent: (agent: AgentStatus) => void;
  removeAgent: (agentId: string) => void;
  setEnvelopeQueueSize: (size: number) => void;
  pushAudit: (entry: AuditEntry) => void;
  setIsStepPending: (pending: boolean) => void;
  handleEnvelopeEvent: (event: AirgapEnvelopeEvent) => void;
  handleViolationEvent: (event: SecurityViolationEvent) => void;
  handleYieldEvent: (event: AgentYieldEvent) => void;
  handleAirgapResponseEvent: (event: AirgapResponseEvent) => void;
  clearAll: () => void;
}

const MAX_AUDIT = 500;

function makeAuditId(): string {
  return `audit:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`;
}

function appendAudit(log: AuditEntry[], entry: AuditEntry): AuditEntry[] {
  return log.length >= MAX_AUDIT ? [...log.slice(1), entry] : [...log, entry];
}

export const useReasoningStore = create<ReasoningState>((set, get) => ({
  agents: [],
  engineReady: false,
  envelopeQueueSize: 0,
  auditLog: [],
  isStepPending: false,

  setEngineReady: (ready) => set({ engineReady: ready }),
  setIsStepPending: (pending) => set({ isStepPending: pending }),

  upsertAgent: (agent) =>
    set((state) => {
      const idx = state.agents.findIndex((a) => a.agentId === agent.agentId);
      if (idx === -1) {
        return { agents: [...state.agents, agent] };
      }
      const agents = state.agents.slice();
      agents[idx] = agent;
      return { agents };
    }),

  removeAgent: (agentId) =>
    set((state) => ({
      agents: state.agents.filter((a) => a.agentId !== agentId),
    })),

  setEnvelopeQueueSize: (size) => set({ envelopeQueueSize: size }),

  pushAudit: (entry) =>
    set((state) => ({ auditLog: appendAudit(state.auditLog, entry) })),

  handleEnvelopeEvent: (event) =>
    set((state) => {
      const entry: AuditEntry = {
        id: makeAuditId(),
        timestamp: Date.now(),
        kind: "envelope",
        agentId: event.agentId,
        message: `airgap_send seq=${event.seq} op=${event.op}`,
      };
      const idx = state.agents.findIndex(
        (a) => a.agentId === event.agentId || a.agentId.endsWith(`:${event.agentId}`)
      );
      const existing = idx >= 0 ? state.agents[idx] : undefined;
      const updatedAgent: AgentStatus = existing
        ? {
            ...existing,
            state: "running",
            envelopesSent: event.seq + 1,
          }
        : {
            agentId: event.agentId,
            state: "running",
            fuelRemaining: 0,
            fuelBudget: 0,
            envelopesSent: event.seq + 1,
            yieldMessage: "",
          };

      const agents = idx >= 0
        ? state.agents.map((a, i) => (i === idx ? updatedAgent : a))
        : [...state.agents, updatedAgent];

      return {
        auditLog: appendAudit(state.auditLog, entry),
        agents,
      };
    }),

  handleViolationEvent: (event) => {
    const entry: AuditEntry = {
      id: makeAuditId(),
      timestamp: Date.now(),
      kind: "violation",
      agentId: event.agentId,
      message: `SECURITY ${event.op}: ${event.violation}`,
    };
    get().pushAudit(entry);
  },

  handleYieldEvent: (event) =>
    set((state) => {
      const entry: AuditEntry = {
        id: makeAuditId(),
        timestamp: Date.now(),
        kind: "yield",
        agentId: event.agentId,
        message: event.yieldMessage
          ? `yielded: ${event.yieldMessage}`
          : "agent yielded",
      };
      const idx = state.agents.findIndex(
        (a) => a.agentId === event.agentId || a.agentId.endsWith(`:${event.agentId}`)
      );
      const existing = idx >= 0 ? state.agents[idx] : undefined;
      const updatedAgent: AgentStatus = existing
        ? {
            ...existing,
            state: "yielded",
            envelopesSent: event.envelopesSent,
            yieldMessage: event.yieldMessage,
          }
        : {
            agentId: event.agentId,
            state: "yielded",
            fuelRemaining: 0,
            fuelBudget: 0,
            envelopesSent: event.envelopesSent,
            yieldMessage: event.yieldMessage,
          };

      const agents = idx >= 0
        ? state.agents.map((a, i) => (i === idx ? updatedAgent : a))
        : [...state.agents, updatedAgent];

      return {
        auditLog: appendAudit(state.auditLog, entry),
        agents,
      };
    }),

  handleAirgapResponseEvent: (event) => {
    const isSuccess = event.isSuccess ?? (event as any).is_success;
    const entry: AuditEntry = {
      id: makeAuditId(),
      timestamp: Date.now(),
      kind: "step",
      agentId: event.agentId,
      message: `DEL response seq=${event.seq} op=${event.op} ${isSuccess ? "success" : "error"}`,
    };
    get().pushAudit(entry);
  },

  clearAll: () =>
    set({
      agents: [],
      engineReady: false,
      envelopeQueueSize: 0,
      auditLog: [],
      isStepPending: false,
    }),
}));